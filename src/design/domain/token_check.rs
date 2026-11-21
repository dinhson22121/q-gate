//! Token: hard-coded colours / px spacing in added lines where the design binds a variable.

use crate::review::application::{DesignMismatch, DesignMismatchKind};
use crate::review::domain::change_set::FileChange;

use super::checks::mismatch;
use super::spec::{DesignSpec, TokenKind, TokenRef};
use super::ui::{added_lines, ignored_lines};

const MAX_TOKEN_MISMATCHES: usize = 30;
const COLOR_FUNCTIONS: [&str; 4] = ["rgba(", "rgb(", "hsla(", "hsl("];
/// A px literal only counts as spacing on a line mentioning one of these.
const SPACING_HINTS: [&str; 10] =
    ["padding", "margin", "gap", "inset", "top", "left", "right", "bottom", "spacing", "space"];
/// Hairlines and resets are not design tokens.
const TRIVIAL_PX: [f64; 2] = [0.0, 1.0];

/// One literal found in code.
#[derive(Debug, PartialEq)]
struct Literal {
    raw: String,
    kind: TokenKind,
    /// `#rrggbb` for colours that can be resolved, the number for px.
    value: Option<String>,
}

pub(super) fn check(spec: &DesignSpec, files: &[&FileChange]) -> Vec<DesignMismatch> {
    let colors: Vec<&TokenRef> = spec.tokens_of(TokenKind::Color).collect();
    let spacing: Vec<&TokenRef> = spec.tokens_of(TokenKind::Spacing).collect();
    if colors.is_empty() && spacing.is_empty() {
        return Vec::new();
    }
    let mut found = Vec::new();
    for file in files {
        let ignored = ignored_lines(file);
        for (line, code) in added_lines(file) {
            if ignored.contains(&line) || is_comment_or_variable(code) {
                continue;
            }
            for literal in literals(code) {
                let tokens = if literal.kind == TokenKind::Color { &colors } else { &spacing };
                if let Some(token) = closest(&literal, tokens) {
                    found.push(report(spec, file, line, code, &literal, token));
                }
            }
        }
    }
    found.truncate(MAX_TOKEN_MISMATCHES);
    found
}

fn report(
    spec: &DesignSpec,
    file: &FileChange,
    line: u32,
    code: &str,
    literal: &Literal,
    token: &TokenRef,
) -> DesignMismatch {
    let what = if literal.kind == TokenKind::Color { "colour" } else { "spacing" };
    let relation = if literal.value.as_deref().is_some_and(|v| same_value(v, &token.value, literal.kind)) {
        "matches"
    } else {
        "should use"
    };
    let explanation = format!(
        "Hard-coded {what} `{}` where the design binds a variable: it {relation} `{}` ({}). Use the design token \
         instead of a literal.",
        literal.raw, token.name, token.value
    );
    mismatch(
        DesignMismatchKind::Token,
        spec,
        format!("{} = {}", token.name, token.value),
        (file, line, code),
        explanation,
    )
}

fn is_comment_or_variable(code: &str) -> bool {
    let trimmed = code.trim_start();
    ["//", "/*", "*", "<!--"].iter().any(|p| trimmed.starts_with(p)) || code.contains("var(")
}

/// The token with the same value, else the nearest one (colour distance / px difference).
fn closest<'a>(literal: &Literal, tokens: &[&'a TokenRef]) -> Option<&'a TokenRef> {
    let first = tokens.first().copied()?;
    let Some(value) = &literal.value else { return Some(first) };
    let distance = |token: &TokenRef| match literal.kind {
        TokenKind::Color => color_distance(value, &token.value),
        _ => number_distance(value, &token.value),
    };
    Some(tokens.iter().copied().min_by(|a, b| distance(a).total_cmp(&distance(b))).unwrap_or(first))
}

fn same_value(a: &str, b: &str, kind: TokenKind) -> bool {
    match kind {
        TokenKind::Color => color_distance(a, b) == 0.0,
        _ => number_distance(a, b) == 0.0,
    }
}

fn color_distance(a: &str, b: &str) -> f64 {
    match (rgb_of(a), rgb_of(b)) {
        (Some(x), Some(y)) => x.iter().zip(y).map(|(p, q)| (f64::from(*p) - f64::from(q)).powi(2)).sum(),
        _ => f64::MAX,
    }
}

fn number_distance(a: &str, b: &str) -> f64 {
    match (a.trim_end_matches("px").parse::<f64>(), b.trim_end_matches("px").parse::<f64>()) {
        (Ok(x), Ok(y)) => (x - y).abs(),
        _ => f64::MAX,
    }
}

/// `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa` → [r, g, b] (alpha ignored).
fn rgb_of(hex: &str) -> Option<[u8; 3]> {
    let digits = hex.strip_prefix('#')?;
    let full: String = match digits.len() {
        3 | 4 => digits.chars().take(3).flat_map(|c| [c, c]).collect(),
        6 | 8 => digits[..6].to_string(),
        _ => return None,
    };
    let channel = |i: usize| u8::from_str_radix(full.get(i..i + 2)?, 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

fn literals(code: &str) -> Vec<Literal> {
    let mut found = hex_colors(code);
    found.extend(color_functions(code));
    found.extend(px_spacing(code));
    found
}

fn boundary_before(code: &str, index: usize) -> bool {
    code[..index].chars().next_back().is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '&')
}

fn hex_colors(code: &str) -> Vec<Literal> {
    let mut found = Vec::new();
    for (index, _) in code.match_indices('#') {
        let digits: String = code[index + 1..].chars().take_while(char::is_ascii_hexdigit).collect();
        let next = code[index + 1 + digits.len()..].chars().next();
        let ends = next.is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_');
        if boundary_before(code, index) && ends && matches!(digits.len(), 3 | 4 | 6 | 8) {
            let raw = format!("#{digits}");
            let value = rgb_of(&raw).map(|[r, g, b]| format!("#{r:02x}{g:02x}{b:02x}"));
            found.push(Literal { raw, kind: TokenKind::Color, value });
        }
    }
    found
}

fn color_functions(code: &str) -> Vec<Literal> {
    let lower = code.to_ascii_lowercase();
    let mut found = Vec::new();
    for function in COLOR_FUNCTIONS {
        for (index, _) in lower.match_indices(function) {
            if !boundary_before(&lower, index) {
                continue;
            }
            let Some(close) = lower[index..].find(')') else { continue };
            let raw = code[index..=index + close].to_string();
            let value = function.starts_with("rgb").then(|| rgb_function_hex(&raw)).flatten();
            found.push(Literal { raw, kind: TokenKind::Color, value });
        }
    }
    found
}

fn rgb_function_hex(raw: &str) -> Option<String> {
    let inner = raw.split_once('(')?.1.trim_end_matches(')');
    let parts: Vec<u8> =
        inner.split([',', ' ', '/']).filter(|p| !p.is_empty()).take(3).filter_map(|p| p.parse().ok()).collect();
    match parts.as_slice() {
        [r, g, b] => Some(format!("#{r:02x}{g:02x}{b:02x}")),
        _ => None,
    }
}

fn px_spacing(code: &str) -> Vec<Literal> {
    let lower = code.to_ascii_lowercase();
    let hinted = SPACING_HINTS.iter().any(|h| lower.contains(h));
    let mut found = Vec::new();
    for (index, _) in lower.match_indices("px") {
        let before = &lower[..index];
        let number: String = before.chars().rev().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        let number: String = number.chars().rev().collect();
        let start = index - number.len();
        let after_ok = lower[index + 2..].chars().next().is_none_or(|c| !c.is_ascii_alphanumeric());
        let Ok(value) = number.parse::<f64>() else { continue };
        let tailwind = before[..start].ends_with("-[");
        if after_ok && boundary_before(&lower, start) && (hinted || tailwind) && !TRIVIAL_PX.contains(&value) {
            found.push(Literal { raw: format!("{number}px"), kind: TokenKind::Spacing, value: Some(number) });
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::domain::fixtures::{spec, ui_file};

    fn token(name: &str, value: &str, kind: TokenKind) -> TokenRef {
        TokenRef { name: name.into(), value: value.into(), kind }
    }

    fn design() -> DesignSpec {
        let mut s = spec(&[]);
        s.tokens = vec![
            token("color/brand/primary", "#3366ff", TokenKind::Color),
            token("color/text/danger", "#d92d20", TokenKind::Color),
            token("space/md", "16", TokenKind::Spacing),
        ];
        s
    }

    #[test]
    fn finds_colour_and_px_literals() {
        let kinds = |code: &str| literals(code).into_iter().map(|l| l.raw).collect::<Vec<_>>();
        assert_eq!(kinds("color: #36F; background: #3366ffcc;"), vec!["#36F", "#3366ffcc"]);
        assert_eq!(kinds("border: 1px solid rgb(51, 102, 255); padding: 16px 0px"), vec!["rgb(51, 102, 255)", "16px"]);
        assert_eq!(kinds("className=\"p-[12px] text-sm\""), vec!["12px"]);
        assert_eq!(kinds("width: 320px; href=\"#section\" &#39; id#abc"), Vec::<String>::new());
        assert_eq!(kinds("color: HSL(10 20% 30%)"), vec!["HSL(10 20% 30%)"]);
    }

    #[test]
    fn literal_where_design_binds_a_variable_is_reported_with_the_variable() {
        let file = ui_file("src/Pay.tsx", &["  <b style={{ color: '#36f', padding: '12px' }}>x</b>"]);
        let found = check(&design(), &[&file]);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].design_value, "color/brand/primary = #3366ff");
        assert!(found[0].explanation.contains("matches"));
        assert_eq!(found[1].design_value, "space/md = 16");
        assert!(found[1].explanation.contains("should use"));
        assert_eq!(found[0].kind, DesignMismatchKind::Token);
    }

    #[test]
    fn nearest_colour_token_is_suggested() {
        let file = ui_file("a.css", &[".err { color: rgb(220, 40, 30); }"]);
        let found = check(&design(), &[&file]);
        assert_eq!(found[0].design_value, "color/text/danger = #d92d20");
    }

    #[test]
    fn nothing_reported_without_bound_variables_or_on_ignored_lines() {
        let file = ui_file("src/Pay.tsx", &["const c = '#3366ff';"]);
        assert!(check(&spec(&[]), &[&file]).is_empty());
        let ignored = ui_file("src/Pay.tsx", &["const c = '#3366ff'; // qgate-design-ignore"]);
        assert!(check(&design(), &[&ignored]).is_empty());
        let variable = ui_file("a.css", &["color: var(--brand, #3366ff);", "// #fff was the old colour"]);
        assert!(check(&design(), &[&variable]).is_empty());
    }
}
