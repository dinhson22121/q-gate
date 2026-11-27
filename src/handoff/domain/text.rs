use crate::review::application::{HumanMark, UnverifiableReason, UnverifiableSubject};

const MIN_FENCE: usize = 3;

pub fn reason_label(reason: UnverifiableReason) -> String {
    reason.as_str().replace('_', " ")
}

pub fn subject_label(subject: UnverifiableSubject) -> &'static str {
    match subject {
        UnverifiableSubject::Criterion => "criterion",
        UnverifiableSubject::Test => "test",
        UnverifiableSubject::File => "file",
    }
}

pub fn human_label(mark: HumanMark) -> &'static str {
    match mark {
        HumanMark::VerifiedMet => "human-verified",
        HumanMark::NotApplicable => "not applicable",
    }
}

fn longest_run(text: &str, ch: char) -> usize {
    let (mut best, mut current) = (0, 0);
    for c in text.chars() {
        current = if c == ch { current + 1 } else { 0 };
        best = best.max(current);
    }
    best
}

pub fn fenced(language: &str, content: &str) -> String {
    let fence = "`".repeat(MIN_FENCE.max(longest_run(content, '`') + 1));
    let language: String = language.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-')).collect();
    let body = content.trim_end_matches(['\n', '\r']);
    format!("{fence}{language}\n{body}\n{fence}\n")
}

pub fn code_span(text: &str) -> String {
    let text = one_line(text);
    let delimiter = "`".repeat(longest_run(&text, '`') + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') { " " } else { "" };
    format!("{delimiter}{pad}{text}{pad}{delimiter}")
}

pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn cell(text: &str) -> String {
    one_line(text).replace('|', "\\|")
}

pub fn prose(text: &str) -> String {
    text.trim()
        .lines()
        .map(|line| {
            let line = line.trim_end();
            let body = line.trim_start();
            if body.starts_with("```") || body.starts_with("~~~") || body.starts_with('#') {
                format!("{}\\{body}", &line[..line.len() - body.len()])
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn language_of(path: &str) -> &'static str {
    let extension = path.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).unwrap_or_default();
    match extension.as_str() {
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "py" => "python",
        "rs" => "rust",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "jsx",
        "ts" => "typescript",
        "tsx" => "tsx",
        "sql" => "sql",
        "sh" | "bash" => "sh",
        "yml" | "yaml" => "yaml",
        "json" => "json",
        "toml" => "toml",
        "css" | "scss" => "css",
        "html" => "html",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fence_is_longer_than_any_backtick_run() {
        assert_eq!(fenced("go", "x := 1\n"), "```go\nx := 1\n```\n");
        let tricky = "doc:\n````md\n```\n````\n";
        let block = fenced("markdown", tricky);
        assert!(block.starts_with("`````markdown\n") && block.ends_with("\n`````\n"), "{block}");
        assert_eq!(fenced("c#; rm -rf", "x"), "```crm-rf\nx\n```\n", "info string is sanitised");
    }

    #[test]
    fn code_span_handles_backticks_and_newlines() {
        assert_eq!(code_span("src/a.go:3"), "`src/a.go:3`");
        assert_eq!(code_span("a`b"), "``a`b``");
        assert_eq!(code_span("`x`"), "`` `x` ``");
        assert_eq!(code_span("a\nb"), "`a b`");
    }

    #[test]
    fn prose_escapes_fences_and_headings_but_keeps_text() {
        assert_eq!(prose("fine\n  ```rust\n# title\n~~~"), "fine\n  \\```rust\n\\# title\n\\~~~");
        assert_eq!(cell("a | b\nc"), "a \\| b c");
    }

    #[test]
    fn reasons_and_human_marks_have_readable_labels() {
        assert_eq!(reason_label(UnverifiableReason::TestNotRunnable), "test not runnable");
        assert_eq!(human_label(HumanMark::VerifiedMet), "human-verified");
        assert_eq!(human_label(HumanMark::NotApplicable), "not applicable");
    }

    #[test]
    fn languages_from_extensions() {
        assert_eq!(language_of("pay/total.GO"), "go");
        assert_eq!(language_of("Makefile"), "");
        assert_eq!(language_of("web/App.tsx"), "tsx");
        assert_eq!(language_of("a.kt"), "kotlin");
    }
}
