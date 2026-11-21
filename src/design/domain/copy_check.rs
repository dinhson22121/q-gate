//! Copy: text in the frame that appears neither in the changed UI files nor in the i18n resources.

use std::collections::HashSet;

use crate::review::application::{DesignMismatch, DesignMismatchKind};
use crate::review::domain::change_set::FileChange;

use super::checks::mismatch;
use super::spec::DesignSpec;
use super::text::{is_meaningful_copy, normalize, similarity, text_candidates, words};
use super::ui::{added_lines, ignored_lines};

/// Word overlap from which a changed literal counts as "the same copy, written differently".
const SIMILAR_THRESHOLD: f64 = 0.3;
/// Anchor lines shorter than this would not pass the review's evidence check.
const MIN_ANCHOR_CHARS: usize = 8;
const MAX_COPY_MISMATCHES: usize = 25;

/// A string literal (or tag text) on an added, non-ignored line.
struct Candidate<'a> {
    file: &'a FileChange,
    line: u32,
    code: &'a str,
    literal: String,
}

pub(super) fn check(spec: &DesignSpec, files: &[&FileChange], i18n: &str) -> Vec<DesignMismatch> {
    let code = normalize(&files.iter().map(|f| joined_lines(f)).collect::<Vec<_>>().join("\n"));
    let missing: Vec<&String> = spec
        .texts
        .iter()
        .filter(|t| is_meaningful_copy(t))
        .filter(|t| {
            let wanted = normalize(t);
            !code.contains(&wanted) && !i18n.contains(&wanted)
        })
        .take(MAX_COPY_MISMATCHES)
        .collect();
    if missing.is_empty() {
        return Vec::new();
    }
    let candidates = candidates(files);
    let anchor = anchor(spec, files);
    missing.into_iter().filter_map(|text| report(spec, text, &candidates, anchor)).collect()
}

fn joined_lines(file: &FileChange) -> String {
    file.new_lines().into_iter().map(|(_, text)| text).collect::<Vec<_>>().join("\n")
}

fn report(
    spec: &DesignSpec,
    text: &str,
    candidates: &[Candidate<'_>],
    anchor: Option<(&FileChange, u32, &str)>,
) -> Option<DesignMismatch> {
    let best = candidates
        .iter()
        .map(|c| (similarity(text, &c.literal), c))
        .filter(|(score, _)| *score >= SIMILAR_THRESHOLD)
        .max_by(|a, b| a.0.total_cmp(&b.0));
    if let Some((_, c)) = best {
        let explanation =
            format!("The design says \"{text}\" but the code shows \"{}\" (and no i18n resource has it).", c.literal);
        return Some(mismatch(DesignMismatchKind::Copy, spec, text.to_string(), (c.file, c.line, c.code), explanation));
    }
    let explanation = format!(
        "Missing copy: the design text \"{text}\" appears neither in the changed UI files nor in the repository's \
         i18n resources."
    );
    anchor.map(|at| mismatch(DesignMismatchKind::Copy, spec, text.to_string(), at, explanation))
}

fn candidates<'a>(files: &[&'a FileChange]) -> Vec<Candidate<'a>> {
    let mut found = Vec::new();
    for file in files {
        let ignored = ignored_lines(file);
        for (line, code) in added_lines(file).into_iter().filter(|(n, _)| !ignored.contains(n)) {
            for literal in text_candidates(code) {
                found.push(Candidate { file, line, code, literal });
            }
        }
    }
    found
}

/// First substantial added line of the UI file whose code shares the most words with the frame's copy.
fn anchor<'a>(spec: &DesignSpec, files: &[&'a FileChange]) -> Option<(&'a FileChange, u32, &'a str)> {
    let design_words: HashSet<String> = spec.texts.iter().flat_map(|t| words(t)).collect();
    let mut best: Option<(usize, (&FileChange, u32, &str))> = None;
    for file in files {
        let Some((line, code)) = first_anchor_line(file) else { continue };
        let overlap =
            words(&joined_lines(file)).into_iter().collect::<HashSet<_>>().intersection(&design_words).count();
        if best.is_none_or(|(score, _)| overlap > score) {
            best = Some((overlap, (file, line, code)));
        }
    }
    best.map(|(_, at)| at)
}

fn first_anchor_line(file: &FileChange) -> Option<(u32, &str)> {
    let ignored = ignored_lines(file);
    let usable: Vec<(u32, &str)> = added_lines(file).into_iter().filter(|(n, _)| !ignored.contains(n)).collect();
    let substantial = |text: &str| text.chars().filter(|c| !c.is_whitespace()).count() >= MIN_ANCHOR_CHARS;
    usable.iter().find(|(_, text)| substantial(text)).or_else(|| usable.first()).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::domain::fixtures::{file_with, spec, ui_file};

    fn run(texts: &[&str], files: &[FileChange], i18n: &str) -> Vec<DesignMismatch> {
        let refs: Vec<&FileChange> = files.iter().collect();
        check(&spec(texts), &refs, &normalize(i18n))
    }

    #[test]
    fn copy_present_in_code_is_not_reported_even_across_lines() {
        let files = vec![ui_file("src/Pay.tsx", &["<p>", "  Pay   securely", "  now", "</p>"])];
        assert!(run(&["Pay securely now"], &files, "").is_empty());
        let curly = vec![ui_file("src/Pay.tsx", &["<p>Don&apos;t PAY twice</p>"])];
        assert!(run(&["Don\u{2019}t pay twice"], &curly, "").is_empty());
    }

    #[test]
    fn copy_found_in_i18n_resources_is_not_reported() {
        let files = vec![ui_file("src/Pay.tsx", &["<p>{t('pay.cta')}</p>"])];
        assert!(run(&["Pay securely now"], &files, "{\"pay\":{\"cta\":\"Pay securely now\"}}").is_empty());
    }

    #[test]
    fn placeholders_are_ignored() {
        let files = vec![ui_file("src/Pay.tsx", &["<p>Something else entirely</p>"])];
        assert!(run(&["Lorem ipsum dolor sit", "Label", "42", "★"], &files, "").is_empty());
    }

    #[test]
    fn differing_copy_points_at_the_most_similar_literal() {
        let files = vec![ui_file(
            "src/Pay.tsx",
            &["const title = 'Order summary';", "return <Button label=\"Pay later today\" />;"],
        )];
        let found = run(&["Pay securely today"], &files, "");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 2);
        assert_eq!(found[0].code_evidence, "return <Button label=\"Pay later today\" />;");
        assert!(found[0].explanation.contains("Pay later today"));
        assert_eq!(found[0].frame, "Checkout / Payment");
    }

    #[test]
    fn missing_copy_is_anchored_at_first_changed_line_of_most_related_file() {
        let unrelated = ui_file("src/Nav.tsx", &["export const Nav = () => <nav />;"]);
        let related = file_with(
            "src/Checkout.tsx",
            &["// checkout summary"],
            &["x", "export function Checkout() {", "  return <section>checkout summary</section>;"],
        );
        let found = run(&["Your checkout summary", "Shipping is free today"], &[unrelated, related], "");
        assert_eq!(found.len(), 2);
        let missing = found.iter().find(|m| m.design_value == "Shipping is free today").unwrap();
        assert_eq!(missing.file, "src/Checkout.tsx");
        assert_eq!(missing.line, 3);
        assert_eq!(missing.code_evidence, "export function Checkout() {");
        assert!(missing.explanation.starts_with("Missing copy"));
    }

    #[test]
    fn ignore_comment_suppresses_the_line() {
        let files = vec![ui_file(
            "src/Pay.tsx",
            &["<b>Pay later today</b> {/* qgate-design-ignore */}", "<i>unrelated</i>", "export const Total = 1234;"],
        )];
        let found = run(&["Pay securely today"], &files, "");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 3, "lines 1-2 are ignored, so the mismatch falls back to the anchor");
        let all_ignored = vec![ui_file("src/Pay.tsx", &["// qgate-design-ignore", "<b>Pay later today</b>"])];
        assert!(run(&["Pay securely today"], &all_ignored, "").is_empty());
    }
}
