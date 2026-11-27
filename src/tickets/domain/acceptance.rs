const AC_TITLES: &[&str] =
    &["acceptance criteria", "acceptance criterion", "acceptance criterias", "ac", "acs", "definition of done", "dod"];
const SCENARIO_KEYWORDS: &[&str] = &["scenario outline:", "scenario template:", "scenario:", "example:"];
const SCENARIO_CONTINUATIONS: &[&str] = &["examples:", "scenarios:"];
const SECTION_TITLES: &[&str] = &[
    "notes",
    "note",
    "out of scope",
    "context",
    "background",
    "description",
    "summary",
    "design",
    "designs",
    "technical notes",
    "tech notes",
    "implementation notes",
    "dependencies",
    "risks",
    "assumptions",
    "references",
    "links",
    "attachments",
    "test cases",
    "test plan",
    "qa notes",
    "open questions",
    "questions",
    "resources",
    "screenshots",
    "user story",
    "story",
    "goal",
    "goals",
    "scope",
];
const ARROWS: &[&str] = &["->", "=>", "→", "⇒"];
const GHERKIN_JOINER: &str = "; ";
const MAX_HEADING_LEVEL: usize = 6;

pub fn parse_acceptance_criteria(text: &str) -> Vec<String> {
    let lines = classify_all(text);
    let explicit = collect_explicit(&lines);
    if explicit.is_empty() { collect_arrow_items(&lines) } else { explicit }
}

pub fn parse_criteria_list(text: &str) -> Vec<String> {
    let lines = classify_all(text);
    let explicit = collect_explicit(&lines);
    if !explicit.is_empty() {
        return explicit;
    }
    let mut collector = Collector::new(false);
    lines.iter().for_each(|l| collector.feed(l));
    collector.finish()
}

fn collect_explicit(lines: &[Line]) -> Vec<String> {
    let section = lines
        .iter()
        .position(|line| matches!(line, Line::Heading(title) if is_ac_title(title)))
        .map(|heading| collect_section(&lines[heading + 1..]))
        .unwrap_or_default();
    if section.is_empty() { collect_scenarios(lines) } else { section }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    Blank,
    Heading(String),
    Label(String),
    ListItem(String),
    Scenario(String),
    Step { text: String, step: Step },
    ScenarioDetail(String),
    Text(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Given,
    When,
    Then,
    Conjunction,
}

fn classify_all(text: &str) -> Vec<Line> {
    let is_wiki = text.lines().any(|l| wiki_heading(l.trim()).is_some());
    text.lines().map(|line| classify(line.trim(), is_wiki)).collect()
}

fn classify(line: &str, is_wiki: bool) -> Line {
    if line.is_empty() {
        return Line::Blank;
    }
    if let Some(title) = wiki_heading(line).or_else(|| markdown_heading(line, is_wiki)).or_else(|| bold_heading(line)) {
        return Line::Heading(title);
    }
    if let Some(item) = list_item(line, is_wiki) {
        return if item.is_empty() { Line::Blank } else { Line::ListItem(item) };
    }
    if let Some(gherkin) = gherkin_line(line) {
        return gherkin;
    }
    match line.strip_suffix(':') {
        Some(title) => colon_line(title.trim()),
        None if is_ac_title(line) => Line::Heading(line.to_string()),
        None => Line::Text(line.to_string()),
    }
}

fn colon_line(title: &str) -> Line {
    if is_ac_title(title) || is_section_title(title) {
        Line::Heading(title.to_string())
    } else {
        Line::Label(title.to_string())
    }
}

fn wiki_heading(line: &str) -> Option<String> {
    let rest = line.strip_prefix('h').or_else(|| line.strip_prefix('H'))?;
    let mut chars = rest.chars();
    let level = chars.next()?.to_digit(10)? as usize;
    let title = chars.as_str().strip_prefix('.')?;
    ((1..=MAX_HEADING_LEVEL).contains(&level) && (title.is_empty() || title.starts_with(' ')))
        .then(|| title.trim().to_string())
}

fn markdown_heading(line: &str, is_wiki: bool) -> Option<String> {
    if is_wiki {
        return None;
    }
    let level = line.bytes().take_while(|&b| b == b'#').count();
    let title = &line[level..];
    ((1..=MAX_HEADING_LEVEL).contains(&level) && (title.is_empty() || title.starts_with(' ')))
        .then(|| title.trim().trim_end_matches('#').trim().to_string())
}

fn bold_heading(line: &str) -> Option<String> {
    let line = line.strip_suffix(':').unwrap_or(line).trim_end();
    ["**", "__", "*"].iter().find_map(|marker| {
        let inner = line.strip_prefix(marker)?.strip_suffix(marker)?;
        let is_wrapped = !inner.is_empty() && !inner.starts_with(' ') && !inner.contains(marker);
        is_wrapped.then(|| inner.trim().to_string())
    })
}

fn list_item(line: &str, is_wiki: bool) -> Option<String> {
    let rest = strip_bullet(line, is_wiki).or_else(|| strip_ordinal(line)).or_else(|| strip_checkbox(line))?;
    let rest = strip_checkbox(rest).unwrap_or(rest);
    Some(rest.trim().to_string())
}

fn strip_bullet(line: &str, is_wiki: bool) -> Option<&str> {
    let marker = line.chars().next().filter(|c| matches!(c, '-' | '*' | '+') || (is_wiki && *c == '#'))?;
    let rest = line.trim_start_matches(marker);
    (rest.is_empty() || rest.starts_with(' ')).then_some(rest)
}

fn strip_ordinal(line: &str) -> Option<&str> {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    let rest = line[digits..].strip_prefix('.').or_else(|| line[digits..].strip_prefix(')'))?;
    (digits > 0 && (rest.is_empty() || rest.starts_with(' '))).then_some(rest)
}

fn strip_checkbox(line: &str) -> Option<&str> {
    ["[ ]", "[x]", "[X]"].iter().find_map(|box_| line.trim_start().strip_prefix(box_))
}

fn gherkin_line(line: &str) -> Option<Line> {
    let lower = line.to_ascii_lowercase();
    if SCENARIO_KEYWORDS.iter().any(|k| lower.starts_with(k)) {
        return Some(Line::Scenario(line.to_string()));
    }
    if line.starts_with('|') || SCENARIO_CONTINUATIONS.iter().any(|k| lower.starts_with(k)) {
        return Some(Line::ScenarioDetail(line.to_string()));
    }
    let first_word = lower.split_whitespace().next()?;
    let step = match first_word {
        "given" => Step::Given,
        "when" => Step::When,
        "then" => Step::Then,
        "and" | "but" => Step::Conjunction,
        _ => return None,
    };
    Some(Line::Step { text: line.to_string(), step })
}

fn is_ac_title(title: &str) -> bool {
    title_in(title, AC_TITLES)
}

fn is_section_title(title: &str) -> bool {
    title_in(title, SECTION_TITLES)
}

fn title_in(title: &str, known: &[&str]) -> bool {
    let normalized: String = title
        .trim()
        .trim_end_matches(':')
        .trim_matches(|c: char| c == '*' || c == '_' || c.is_whitespace())
        .to_ascii_lowercase();
    let without_suffix = normalized.split(" (").next().unwrap_or(&normalized).trim();
    known.contains(&without_suffix)
}

#[derive(Debug, Default)]
struct Scenario {
    lines: Vec<String>,
    has_scenario_line: bool,
    has_action: bool,
    has_then: bool,
}

impl Scenario {
    fn text(&self) -> String {
        self.lines.join(GHERKIN_JOINER)
    }

    fn is_complete(&self) -> bool {
        self.has_scenario_line || (self.has_action && self.has_then)
    }
}

#[derive(Debug)]
struct Collector {
    scenarios_only: bool,
    items: Vec<String>,
    paragraphs: Vec<String>,
    scenario: Option<Scenario>,
    continues_item: bool,
    label: Option<String>,
}

impl Collector {
    fn new(scenarios_only: bool) -> Self {
        Self {
            scenarios_only,
            items: Vec::new(),
            paragraphs: Vec::new(),
            scenario: None,
            continues_item: false,
            label: None,
        }
    }

    fn labeled(&self, text: &str) -> String {
        match &self.label {
            Some(label) => format!("{label}: {text}"),
            None => text.to_string(),
        }
    }

    fn feed(&mut self, line: &Line) {
        match line {
            Line::Blank => self.continues_item = false,
            Line::Heading(_) => self.set_label(None),
            Line::Label(label) => self.set_label(Some(label.clone())),
            Line::ListItem(text) => self.push_item(text),
            Line::Scenario(text) => self.start_scenario(Some(text)),
            Line::Step { text, step } => self.push_step(text, *step),
            Line::ScenarioDetail(text) => self.push_detail(text),
            Line::Text(text) => self.push_text(text),
        }
    }

    fn set_label(&mut self, label: Option<String>) {
        self.flush_scenario();
        self.continues_item = false;
        self.label = label;
    }

    fn push_item(&mut self, text: &str) {
        self.flush_scenario();
        if !self.scenarios_only {
            self.items.push(self.labeled(text));
            self.continues_item = true;
        }
    }

    fn push_text(&mut self, text: &str) {
        self.flush_scenario();
        if self.scenarios_only {
            return;
        }
        match self.items.last_mut() {
            Some(last) if self.continues_item => {
                last.push(' ');
                last.push_str(text);
            }
            _ => {
                let paragraph = self.labeled(text);
                self.paragraphs.push(paragraph);
            }
        }
    }

    fn start_scenario(&mut self, scenario_line: Option<&str>) {
        self.flush_scenario();
        self.scenario = Some(Scenario {
            lines: scenario_line.map(str::to_string).into_iter().collect(),
            has_scenario_line: scenario_line.is_some(),
            ..Scenario::default()
        });
    }

    fn push_step(&mut self, text: &str, step: Step) {
        let starts_new = match &self.scenario {
            None => step != Step::Conjunction,
            Some(current) => step == Step::Given && current.has_then,
        };
        if starts_new {
            self.start_scenario(None);
        }
        let Some(scenario) = self.scenario.as_mut() else {
            return self.push_text(text);
        };
        scenario.lines.push(text.to_string());
        scenario.has_action |= matches!(step, Step::Given | Step::When);
        scenario.has_then |= step == Step::Then;
    }

    fn push_detail(&mut self, text: &str) {
        match self.scenario.as_mut() {
            Some(scenario) => scenario.lines.push(text.to_string()),
            None => self.push_text(text),
        }
    }

    fn flush_scenario(&mut self) {
        let Some(scenario) = self.scenario.take() else {
            return;
        };
        self.continues_item = false;
        if !self.scenarios_only || scenario.is_complete() {
            self.items.push(scenario.text());
        }
    }

    fn finish(mut self) -> Vec<String> {
        self.flush_scenario();
        if self.items.is_empty() { self.paragraphs } else { self.items }
    }

    fn finish_items(mut self) -> Vec<String> {
        self.flush_scenario();
        self.items
    }
}

fn collect_section(lines: &[Line]) -> Vec<String> {
    let mut collector = Collector::new(false);
    lines.iter().take_while(|l| !matches!(l, Line::Heading(_))).for_each(|l| collector.feed(l));
    collector.finish()
}

fn collect_arrow_items(lines: &[Line]) -> Vec<String> {
    let mut collector = Collector::new(false);
    lines.iter().for_each(|l| collector.feed(l));
    collector.finish_items().into_iter().filter(|item| ARROWS.iter().any(|a| item.contains(a))).collect()
}

fn collect_scenarios(lines: &[Line]) -> Vec<String> {
    let mut collector = Collector::new(true);
    lines.iter().for_each(|l| collector.feed(l));
    collector.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_heading_with_bullets_until_next_heading() {
        let text = "Intro text\n\n## Acceptance Criteria\n- User can export CSV\n- Export is\n  audited\n* Works offline\n\n## Notes\n- not a criterion";
        assert_eq!(parse_acceptance_criteria(text), vec!["User can export CSV", "Export is audited", "Works offline"]);
    }

    #[test]
    fn jira_wiki_heading_with_numbered_and_nested_items() {
        let text = "h2. Context\nSome context\nh3. Acceptance criteria\n# First thing\n## Nested thing\n** bullet thing\nh3. Out of scope\n# nope";
        assert_eq!(parse_acceptance_criteria(text), vec!["First thing", "Nested thing", "bullet thing"]);
    }

    #[test]
    fn bold_and_plain_colon_headings() {
        assert_eq!(parse_acceptance_criteria("*Acceptance Criteria*\n* one\n* two"), vec!["one", "two"]);
        assert_eq!(parse_acceptance_criteria("**AC**:\n1. one\n2) two\nNotes:\n- x"), vec!["one", "two"]);
        assert_eq!(
            parse_acceptance_criteria("Definition of Done:\n- [ ] tests\n- [x] docs\n[ ] demo"),
            vec!["tests", "docs", "demo"]
        );
        assert_eq!(parse_acceptance_criteria("Acceptance Criteria (AC)\n- a"), vec!["a"]);
    }

    #[test]
    fn adf_converted_text() {
        let text = "Login should be fast.\n# Acceptance Criteria\n- Page loads under 1s\n- [ ] Errors are shown\n# Design\nsee figma";
        assert_eq!(parse_acceptance_criteria(text), vec!["Page loads under 1s", "Errors are shown"]);
    }

    #[test]
    fn paragraphs_used_when_section_has_no_list() {
        assert_eq!(
            parse_acceptance_criteria("AC:\nThe user can log in.\nThe session lasts 1h."),
            vec!["The user can log in.", "The session lasts 1h."]
        );
    }

    #[test]
    fn intro_paragraph_dropped_when_list_follows() {
        assert_eq!(parse_acceptance_criteria("## AC\nThe following must hold\n\n- a\n- b"), vec!["a", "b"]);
    }

    #[test]
    fn gherkin_scenarios_in_section_grouped_per_scenario() {
        let text = "## Acceptance Criteria\nScenario: Login ok\n  Given a user\n  When they log in\n  Then they see the dashboard\n\nScenario: Login fails\n  Given a locked user\n  When they log in\n  Then an error is shown\n  And nothing else";
        assert_eq!(
            parse_acceptance_criteria(text),
            vec![
                "Scenario: Login ok; Given a user; When they log in; Then they see the dashboard",
                "Scenario: Login fails; Given a locked user; When they log in; Then an error is shown; And nothing else"
            ]
        );
    }

    #[test]
    fn gherkin_without_scenario_lines_split_on_given_after_then() {
        let text = "AC:\nGiven a\nWhen b\nThen c\nGiven d\nThen e\n- plain item";
        assert_eq!(parse_acceptance_criteria(text), vec!["Given a; When b; Then c", "Given d; Then e", "plain item"]);
    }

    #[test]
    fn gherkin_fallback_without_heading() {
        let text = "Some description.\nGiven the outage, we need this.\n\nScenario Outline: export\n  Given <rows>\n  Then a file\n  Examples:\n  | rows |\n  | 1 |\nMore prose";
        assert_eq!(
            parse_acceptance_criteria(text),
            vec!["Scenario Outline: export; Given <rows>; Then a file; Examples:; | rows |; | 1 |"]
        );
    }

    #[test]
    fn empty_when_nothing_parsable() {
        assert!(parse_acceptance_criteria("Just a description\n- with a list\n").is_empty());
        assert!(parse_acceptance_criteria("").is_empty());
        assert!(parse_acceptance_criteria("## Acceptance Criteria\n## Next").is_empty());
    }

    #[test]
    fn conjunction_without_scenario_is_text_and_detail_without_scenario_is_text() {
        assert_eq!(parse_acceptance_criteria("AC:\nAnd also this\n| a |"), vec!["And also this", "| a |"]);
    }

    #[test]
    fn criteria_list_uses_items_without_heading() {
        assert_eq!(parse_criteria_list("- one\n- two"), vec!["one", "two"]);
        assert_eq!(parse_criteria_list("one\ntwo"), vec!["one", "two"]);
        assert_eq!(parse_criteria_list("h3. AC\n# one"), vec!["one"]);
        assert_eq!(parse_criteria_list("## Scope\n- a"), vec!["a"]);
        assert!(parse_criteria_list("").is_empty());
    }

    #[test]
    fn label_inside_ac_section_prefixes_items() {
        let text = "## Acceptance Criteria\nlogin:\n- Input username password corret -> Login success.\n- Input usename/password wrong -> login fail and show message Invalid credential.";
        assert_eq!(
            parse_acceptance_criteria(text),
            vec![
                "login: Input username password corret -> Login success.",
                "login: Input usename/password wrong -> login fail and show message Invalid credential."
            ]
        );
    }

    #[test]
    fn multiple_labels_each_prefix_their_items() {
        let text = "AC:\nlogin:\n- a\n- b\nlogout:\n- c";
        assert_eq!(parse_acceptance_criteria(text), vec!["login: a", "login: b", "logout: c"]);
    }

    #[test]
    fn label_then_known_section_heading_ends_section() {
        let text = "AC:\nlogin:\n- a\nNotes:\n- n\nDesign:\n- d";
        assert_eq!(parse_acceptance_criteria(text), vec!["login: a"]);
    }

    #[test]
    fn label_without_items_yields_nothing() {
        assert!(parse_acceptance_criteria("AC:\nlogin:\nlogout:").is_empty());
    }

    #[test]
    fn label_paragraph_fallback_is_prefixed() {
        assert_eq!(parse_acceptance_criteria("AC:\nlogin:\nUser sees dashboard"), vec!["login: User sees dashboard"]);
    }

    #[test]
    fn section_titles_are_headings_and_other_colon_lines_are_labels() {
        assert_eq!(classify("Out of scope (v1):", false), Line::Heading("Out of scope (v1)".into()));
        assert_eq!(classify("Acceptance Criteria:", false), Line::Heading("Acceptance Criteria".into()));
        assert_eq!(classify("login:", false), Line::Label("login".into()));
        assert_eq!(classify("**login**:", false), Line::Heading("login".into()));
        assert_eq!(classify("h3. login:", true), Line::Heading("login:".into()));
    }

    #[test]
    fn arrow_fallback_with_labels_ignores_later_notes() {
        let text = "login:\n- a -> b\n- c => d\n\nNotes:\n- remember cache";
        assert_eq!(parse_acceptance_criteria(text), vec!["login: a -> b", "login: c => d"]);
    }

    #[test]
    fn arrow_fallback_ignores_non_arrow_bullets() {
        let text = "Description\n- plain bullet\n- input x -> output y\n- another plain";
        assert_eq!(parse_acceptance_criteria(text), vec!["input x -> output y"]);
    }

    #[test]
    fn arrow_fallback_empty_when_no_arrows() {
        assert!(parse_acceptance_criteria("login:\n- a\n- b").is_empty());
    }

    #[test]
    fn arrow_fallback_supports_unicode_arrows() {
        assert_eq!(parse_acceptance_criteria("- a → b\n- c ⇒ d\n- e"), vec!["a → b", "c ⇒ d"]);
    }

    #[test]
    fn gherkin_wins_over_arrow_items() {
        let text = "- a -> b\nScenario: s\n  Given x\n  Then y";
        assert_eq!(parse_acceptance_criteria(text), vec!["Scenario: s; Given x; Then y"]);
    }

    #[test]
    fn ac_section_wins_over_arrow_items() {
        assert_eq!(parse_acceptance_criteria("- a -> b\nAC:\n- plain"), vec!["plain"]);
    }

    #[test]
    fn criteria_list_with_label_prefixes_every_item() {
        assert_eq!(parse_criteria_list("login:\n- a\n- b"), vec!["login: a", "login: b"]);
    }

    #[test]
    fn criteria_list_without_section_keeps_non_arrow_items() {
        assert_eq!(parse_criteria_list("- a -> b\n- c"), vec!["a -> b", "c"]);
    }

    #[test]
    fn heading_helpers_reject_look_alikes() {
        assert_eq!(wiki_heading("h7. x"), None);
        assert_eq!(wiki_heading("hello"), None);
        assert_eq!(markdown_heading("#hashtag", false), None);
        assert_eq!(markdown_heading("####### x", false), None);
        assert_eq!(bold_heading("* item"), None);
        assert_eq!(bold_heading("*a* and *b*"), None);
        assert_eq!(classify("#tag", false), Line::Text("#tag".into()));
        assert_eq!(classify("12 apples", false), Line::Text("12 apples".into()));
        assert_eq!(classify("---", false), Line::Blank);
    }
}
