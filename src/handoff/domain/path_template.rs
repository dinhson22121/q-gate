//! Review-file paths: the `output.path_template` placeholders, value sanitisation and the matcher that
//! recognises files generated from the same template (the only files pruning may delete).

use chrono::{DateTime, Utc};

const MAX_VALUE_LEN: usize = 80;
const SHORT_SHA_LEN: usize = 7;
const MIN_FULL_SHA_LEN: usize = 40;
const MAX_FULL_SHA_LEN: usize = 64;
const DATE_FORMAT: &str = "%Y%m%d-%H%M%S";
const DATE_DIGITS: (usize, usize) = (8, 6);
const MD_EXTENSION: &str = ".md";
const STATUSES: [&str; 2] = ["passed", "failed"];
pub const NO_TICKET: &str = "no-ticket";
pub const DETACHED_BRANCH: &str = "detached";
const UNKNOWN: &str = "unknown";
/// Stem of `latest.md` / `latest.json`; never treated as a generated per-commit file.
pub const LATEST_STEM: &str = "latest";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placeholder {
    Repo,
    Branch,
    Sha7,
    Sha,
    Date,
    Ticket,
    Status,
}

impl Placeholder {
    pub const ALL: [Self; 7] =
        [Self::Repo, Self::Branch, Self::Sha7, Self::Sha, Self::Date, Self::Ticket, Self::Status];

    pub fn name(self) -> &'static str {
        match self {
            Self::Repo => "repo",
            Self::Branch => "branch",
            Self::Sha7 => "sha7",
            Self::Sha => "sha",
            Self::Date => "date",
            Self::Ticket => "ticket",
            Self::Status => "status",
        }
    }

    /// Placeholders that differ between commits, so a file name containing one is unique per run.
    fn is_run_unique(self) -> bool {
        matches!(self, Self::Sha7 | Self::Sha | Self::Date)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Literal(String),
    Var(Placeholder),
}

/// A validated, relative `.md` path template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathTemplate {
    dirs: Vec<Vec<Part>>,
    /// File name without the `.md` extension.
    stem: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid review file template: {0}")]
pub struct TemplateError(String);

/// Sanitised placeholder values for one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathValues {
    pub repo: String,
    pub branch: String,
    pub sha7: String,
    pub sha: String,
    pub date: String,
    pub ticket: String,
    pub status: String,
}

impl PathValues {
    pub fn new(
        repo: &str,
        branch: Option<&str>,
        commit: &str,
        ticket: Option<&str>,
        at: DateTime<Utc>,
        passed: bool,
    ) -> Self {
        let sha: String = commit
            .chars()
            .filter(char::is_ascii_hexdigit)
            .map(|c| c.to_ascii_lowercase())
            .take(MAX_FULL_SHA_LEN)
            .collect();
        let sha = if sha.is_empty() { UNKNOWN.to_string() } else { sha };
        Self {
            repo: sanitize(repo, UNKNOWN),
            branch: sanitize(branch.unwrap_or(DETACHED_BRANCH), DETACHED_BRANCH),
            sha7: sha.chars().take(SHORT_SHA_LEN).collect(),
            sha,
            date: at.format(DATE_FORMAT).to_string(),
            ticket: sanitize(ticket.unwrap_or(NO_TICKET), NO_TICKET),
            status: STATUSES[usize::from(!passed)].to_string(),
        }
    }

    fn get(&self, placeholder: Placeholder) -> &str {
        match placeholder {
            Placeholder::Repo => &self.repo,
            Placeholder::Branch => &self.branch,
            Placeholder::Sha7 => &self.sha7,
            Placeholder::Sha => &self.sha,
            Placeholder::Date => &self.date,
            Placeholder::Ticket => &self.ticket,
            Placeholder::Status => &self.status,
        }
    }

    /// Folder of `latest.md`: `<repo>/<branch>`.
    pub fn latest_dir(&self) -> String {
        format!("{}/{}", self.repo, self.branch)
    }
}

/// Keeps `[A-Za-z0-9._-]` (others become `-`), drops leading dots, caps the length; `fallback` when empty.
pub fn sanitize(value: &str, fallback: &str) -> String {
    let mapped: String = value
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' })
        .collect();
    let cleaned: String = mapped.trim_start_matches('.').chars().take(MAX_VALUE_LEN).collect();
    if cleaned.is_empty() { fallback.to_string() } else { cleaned }
}

/// A rendered path: `dir` (no trailing slash, may be empty) and the file stem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedPath {
    pub dir: String,
    pub stem: String,
}

impl RenderedPath {
    pub fn file(&self, extension: &str) -> String {
        join(&self.dir, &format!("{}.{extension}", self.stem))
    }
}

/// `dir/name`, or `name` when `dir` is empty.
pub fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") }
}

impl PathTemplate {
    pub fn parse(template: &str) -> Result<Self, TemplateError> {
        let err = |m: &str| TemplateError(format!("'{template}' {m}"));
        let body = template.strip_suffix(MD_EXTENSION).ok_or_else(|| err("must end with .md"))?;
        if template.starts_with('/') || template.contains(['\\', ':', '\0']) {
            return Err(err("must be a relative path using '/'"));
        }
        let mut segments: Vec<Vec<Part>> = Vec::new();
        for segment in body.split('/') {
            segments.push(parse_segment(segment).map_err(|m| err(&m))?);
        }
        let stem = segments.pop().unwrap_or_default();
        if stem.is_empty() {
            return Err(err("needs a file name before .md"));
        }
        let invalid = |parts: &[Part]| parts.is_empty() || matches!(parts, [Part::Literal(l)] if l == "." || l == "..");
        if segments.iter().any(|s| invalid(s)) {
            return Err(err("must not contain empty, '.' or '..' path segments"));
        }
        if matches!(stem.as_slice(), [Part::Literal(l)] if l == LATEST_STEM) {
            return Err(err("must not be latest.md (reserved)"));
        }
        Ok(Self { dirs: segments, stem })
    }

    pub fn render(&self, values: &PathValues) -> RenderedPath {
        let render = |parts: &[Part]| -> String {
            parts
                .iter()
                .map(|p| match p {
                    Part::Literal(l) => l.as_str(),
                    Part::Var(v) => values.get(*v),
                })
                .collect()
        };
        RenderedPath {
            dir: self.dirs.iter().map(|s| render(s)).collect::<Vec<_>>().join("/"),
            stem: render(&self.stem),
        }
    }

    /// Matches file stems generated from this template in the same folder, or `None` when the file name has no
    /// per-run placeholder ({sha7}, {sha}, {date}): then each run overwrites the same file and nothing is pruned.
    pub fn stem_matcher(&self, values: &PathValues) -> Option<StemMatcher> {
        if !self.stem.iter().any(|p| matches!(p, Part::Var(v) if v.is_run_unique())) {
            return None;
        }
        let parts = self
            .stem
            .iter()
            .map(|p| match p {
                Part::Literal(l) => Matcher::Literal(l.clone()),
                Part::Var(v @ (Placeholder::Repo | Placeholder::Branch)) => Matcher::Literal(values.get(*v).into()),
                Part::Var(v) => Matcher::Var(*v),
            })
            .collect();
        Some(StemMatcher { parts })
    }
}

fn parse_segment(segment: &str) -> Result<Vec<Part>, String> {
    let mut parts = Vec::new();
    let mut rest = segment;
    while !rest.is_empty() {
        match rest.find(['{', '}']) {
            None => {
                parts.push(Part::Literal(rest.to_string()));
                break;
            }
            Some(i) if rest[i..].starts_with('}') => return Err("has a '}' without a matching '{'".into()),
            Some(i) => {
                if i > 0 {
                    parts.push(Part::Literal(rest[..i].to_string()));
                }
                let after = &rest[i + 1..];
                let close = after.find('}').ok_or("has a '{' without a matching '}'")?;
                let name = &after[..close];
                let placeholder = Placeholder::ALL
                    .into_iter()
                    .find(|p| p.name() == name)
                    .ok_or_else(|| format!("uses the unknown placeholder {{{name}}}"))?;
                parts.push(Part::Var(placeholder));
                rest = &after[close + 1..];
            }
        }
    }
    Ok(parts)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Matcher {
    Literal(String),
    Var(Placeholder),
}

/// Recognises stems of files generated from one template for one repo/branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StemMatcher {
    parts: Vec<Matcher>,
}

impl StemMatcher {
    /// The stem of `file_name` when it is a generated `.md` / `.json` / `.html` file of this template.
    pub fn stem_of<'a>(&self, file_name: &'a str) -> Option<&'a str> {
        let stem = [".md", ".json", ".html"].iter().find_map(|ext| file_name.strip_suffix(ext))?;
        (stem != LATEST_STEM && matches_from(&self.parts, stem)).then_some(stem)
    }
}

fn matches_from(parts: &[Matcher], text: &str) -> bool {
    let Some((first, rest)) = parts.split_first() else { return text.is_empty() };
    candidate_lengths(first, text).into_iter().any(|len| matches_from(rest, &text[len..]))
}

/// Prefix lengths of `text` that `matcher` accepts.
fn candidate_lengths(matcher: &Matcher, text: &str) -> Vec<usize> {
    let run = |pred: fn(&u8) -> bool| text.bytes().take_while(pred).count();
    match matcher {
        Matcher::Literal(l) => text.starts_with(l.as_str()).then_some(l.len()).into_iter().collect(),
        Matcher::Var(Placeholder::Sha7) => {
            (run(is_lower_hex) >= SHORT_SHA_LEN).then_some(SHORT_SHA_LEN).into_iter().collect()
        }
        Matcher::Var(Placeholder::Sha) => (MIN_FULL_SHA_LEN..=run(is_lower_hex).min(MAX_FULL_SHA_LEN)).collect(),
        Matcher::Var(Placeholder::Date) => {
            let (date, time) = DATE_DIGITS;
            let b = text.as_bytes();
            let ok = b.len() > date + time
                && b[..date].iter().all(u8::is_ascii_digit)
                && b[date] == b'-'
                && b[date + 1..=date + time].iter().all(u8::is_ascii_digit);
            ok.then_some(date + 1 + time).into_iter().collect()
        }
        Matcher::Var(Placeholder::Status) => {
            STATUSES.iter().filter(|s| text.starts_with(*s)).map(|s| s.len()).collect()
        }
        Matcher::Var(_) => {
            if text.starts_with('.') {
                return Vec::new();
            }
            (1..=run(is_safe).min(MAX_VALUE_LEN)).collect()
        }
    }
}

fn is_lower_hex(b: &u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(b)
}

fn is_safe(b: &u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    const SHA: &str = "3f9c2ab7d1e04c5b8a6f9e2d1c0b3a4f5e6d7c8b";

    fn values(branch: Option<&str>, ticket: Option<&str>, passed: bool) -> PathValues {
        let at = Utc.with_ymd_and_hms(2026, 11, 20, 14, 30, 5).unwrap();
        PathValues::new("shop-api", branch, SHA, ticket, at, passed)
    }

    #[test]
    fn renders_default_template_with_sanitised_values() {
        let template = PathTemplate::parse("{repo}/{branch}/{sha7}.md").unwrap();
        let path = template.render(&values(Some("feature/ABC-1"), None, false));
        assert_eq!(path, RenderedPath { dir: "shop-api/feature-ABC-1".into(), stem: "3f9c2ab".into() });
        assert_eq!(path.file("md"), "shop-api/feature-ABC-1/3f9c2ab.md");
        assert_eq!(path.file("json"), "shop-api/feature-ABC-1/3f9c2ab.json");
    }

    #[test]
    fn renders_every_placeholder() {
        let template = PathTemplate::parse("{repo}/{date}_{ticket}_{status}_{sha}.md").unwrap();
        let path = template.render(&values(None, Some("SHOP-7"), true));
        assert_eq!(path.dir, "shop-api");
        assert_eq!(path.stem, format!("20261120-143005_SHOP-7_passed_{SHA}"));
        let detached = PathTemplate::parse("{branch}-{ticket}.md").unwrap().render(&values(None, None, true));
        assert_eq!(detached.file("md"), "detached-no-ticket.md");
        assert_eq!(join("", "a.md"), "a.md");
    }

    #[test]
    fn sanitises_values() {
        assert_eq!(sanitize("feature/ABC-1", "x"), "feature-ABC-1");
        assert_eq!(sanitize("../../etc", "x"), "-..-etc");
        assert_eq!(sanitize("..", "fallback"), "fallback");
        assert_eq!(sanitize(".hidden", "x"), "hidden");
        assert_eq!(sanitize("héllo wörld", "x"), "h-llo-w-rld");
        assert_eq!(sanitize(&"a".repeat(200), "x").len(), 80);
        assert_eq!(sanitize("  ", "x"), "x");
        let v = PathValues::new("my repo", Some("a/../b"), "NOT-A-SHA", None, Utc::now(), true);
        assert_eq!((v.repo.as_str(), v.branch.as_str()), ("my-repo", "a-..-b"));
        assert_eq!((v.sha.as_str(), v.sha7.as_str()), ("aa", "aa"), "only hex digits of the commit are kept");
        assert_eq!(PathValues::new("r", None, "", None, Utc::now(), true).sha, "unknown");
        assert_eq!(v.latest_dir(), "my-repo/a-..-b");
    }

    #[test]
    fn rejects_invalid_templates() {
        for bad in [
            "{repo}/{sha7}.txt",
            "/abs/{sha7}.md",
            "a\\b.md",
            "C:{sha7}.md",
            "{repo}/../{sha7}.md",
            "{repo}//{sha7}.md",
            "./{sha7}.md",
            "{repo}/.md",
            "{who}.md",
            "{repo.md",
            "repo}.md",
            "{repo}/latest.md",
        ] {
            assert!(PathTemplate::parse(bad).is_err(), "{bad}");
        }
        let err = PathTemplate::parse("{who}.md").unwrap_err().to_string();
        assert!(err.contains("{who}"), "{err}");
    }

    #[test]
    fn matcher_recognises_only_generated_stems() {
        let v = values(Some("main"), Some("SHOP-7"), false);
        let template = PathTemplate::parse("{repo}/{branch}/{sha7}.md").unwrap();
        let m = template.stem_matcher(&v).unwrap();
        assert_eq!(m.stem_of("3f9c2ab.md"), Some("3f9c2ab"));
        assert_eq!(m.stem_of("0000000.json"), Some("0000000"));
        for other in ["notes.md", "3f9c2ab.txt", "3F9C2AB.md", "3f9c2ab1.md", "latest.md", "3f9c2a.md", ".md"] {
            assert_eq!(m.stem_of(other), None, "{other}");
        }

        let rich = PathTemplate::parse("{date}-{ticket}-{status}-{branch}-{sha}.md").unwrap();
        let m = rich.stem_matcher(&v).unwrap();
        assert!(m.stem_of(&format!("20261120-143005-SHOP-7-failed-main-{SHA}.md")).is_some());
        assert!(m.stem_of(&format!("20261120-143005-OPS-1-passed-main-{SHA}.md")).is_some());
        assert!(m.stem_of(&format!("20261120-143005-SHOP-7-failed-dev-{SHA}.md")).is_none(), "other branch");
        assert!(m.stem_of(&format!("20261120-143005-SHOP-7-weird-main-{SHA}.md")).is_none());
        assert!(m.stem_of("2026112-143005-SHOP-7-failed-main-abc.md").is_none());
    }

    #[test]
    fn no_matcher_without_a_per_run_placeholder() {
        let v = values(Some("main"), None, true);
        assert!(PathTemplate::parse("{repo}/{ticket}.md").unwrap().stem_matcher(&v).is_none());
        assert!(PathTemplate::parse("{repo}/{sha7}/review.md").unwrap().stem_matcher(&v).is_none());
    }
}
