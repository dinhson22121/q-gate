use std::path::Path;

use serde::{Deserialize, Serialize};

use super::validation::{ValidationError, ValidationResult, require_range};

const DEFAULT_OUTPUT_DIR: &str = "/reviews";
const DEFAULT_PATH_TEMPLATE: &str = "{repo}/{branch}/{sha7}.md";
const DEFAULT_KEEP_LAST: u32 = 20;
const MAX_KEEP_LAST: u32 = 1000;
const MAX_TEMPLATE_LEN: usize = 200;
const TEMPLATE_EXTENSION: &str = ".md";
const RESERVED_FILE_NAME: &str = "latest.md";
const DEFAULT_RULES_FILE: &str = "qgate-rules.md";
const DEFAULT_RULES_MAX: u32 = 100;
const MAX_RULES_MAX: u32 = 1000;
const MAX_RULES_FILE_LEN: usize = 100;

pub const OUTPUT_PLACEHOLDERS: [&str; 7] = ["repo", "branch", "sha7", "sha", "date", "ticket", "status"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Markdown,
    Json,
    Html,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputConfig {
    pub enabled: bool,
    pub dir: String,
    pub path_template: String,
    pub write_latest: bool,
    pub formats: Vec<OutputFormat>,
    pub include_passing: bool,
    pub keep_last: u32,
    pub rules_enabled: bool,
    pub rules_file: String,
    pub rules_max: u32,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dir: DEFAULT_OUTPUT_DIR.into(),
            path_template: DEFAULT_PATH_TEMPLATE.into(),
            write_latest: true,
            formats: vec![OutputFormat::Markdown],
            include_passing: true,
            keep_last: DEFAULT_KEEP_LAST,
            rules_enabled: true,
            rules_file: DEFAULT_RULES_FILE.into(),
            rules_max: DEFAULT_RULES_MAX,
        }
    }
}

impl OutputConfig {
    pub fn validate(&self) -> ValidationResult {
        if !Path::new(&self.dir).is_absolute() {
            return Err(ValidationError::new("output.dir", format!("must be an absolute path, got '{}'", self.dir)));
        }
        validate_path_template(&self.path_template)
            .map_err(|message| ValidationError::new("output.path_template", message))?;
        if self.enabled && self.formats.is_empty() {
            return Err(ValidationError::new("output.formats", "pick at least one format or disable review files"));
        }
        require_range("output.keep_last", self.keep_last, 0, MAX_KEEP_LAST)?;
        validate_rules_file(&self.rules_file).map_err(|message| ValidationError::new("output.rules_file", message))?;
        if self.path_template.rsplit('/').next() == Some(self.rules_file.as_str()) {
            return Err(ValidationError::new("output.rules_file", "must differ from the review file name"));
        }
        require_range("output.rules_max", self.rules_max, 1, MAX_RULES_MAX)
    }
}

pub fn validate_rules_file(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_RULES_FILE_LEN {
        return Err(format!("must be 1 to {MAX_RULES_FILE_LEN} characters"));
    }
    if !name.ends_with(TEMPLATE_EXTENSION) || name.len() == TEMPLATE_EXTENSION.len() {
        return Err(format!("must be a file name ending with {TEMPLATE_EXTENSION}"));
    }
    if name.starts_with('.') || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) {
        return Err("must be a plain file name (letters, digits, '.', '_', '-'; no folders, not hidden)".into());
    }
    if name == RESERVED_FILE_NAME {
        return Err(format!("{RESERVED_FILE_NAME} is reserved for the latest review"));
    }
    Ok(())
}

pub fn validate_path_template(template: &str) -> Result<(), String> {
    if template.trim().is_empty() {
        return Err("must not be empty".into());
    }
    if template.len() > MAX_TEMPLATE_LEN {
        return Err(format!("must be at most {MAX_TEMPLATE_LEN} characters"));
    }
    if !template.ends_with(TEMPLATE_EXTENSION) {
        return Err(format!("must end with {TEMPLATE_EXTENSION}"));
    }
    if template.starts_with('/') || template.contains('\\') || template.contains(':') {
        return Err("must be a relative path using '/' (no leading '/', '\\' or ':')".into());
    }
    for segment in template.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err("must not contain empty, '.' or '..' path segments".into());
        }
    }
    if template.rsplit('/').next() == Some(RESERVED_FILE_NAME) {
        return Err(format!("{RESERVED_FILE_NAME} is reserved for the latest review"));
    }
    check_placeholders(template)
}

fn check_placeholders(template: &str) -> Result<(), String> {
    let mut rest = template;
    while let Some(open) = rest.find(['{', '}']) {
        if rest[open..].starts_with('}') {
            return Err("has a '}' without a matching '{'".into());
        }
        let after = &rest[open + 1..];
        let close = after.find('}').ok_or("has a '{' without a matching '}'")?;
        let name = &after[..close];
        if !OUTPUT_PLACEHOLDERS.contains(&name) {
            return Err(format!(
                "unknown placeholder {{{name}}}; use {}",
                OUTPUT_PLACEHOLDERS.map(|p| format!("{{{p}}}")).join(" ")
            ));
        }
        rest = &after[close + 1..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_markdown_only() {
        let config = OutputConfig::default();
        assert!(config.validate().is_ok());
        assert_eq!(config.formats, vec![OutputFormat::Markdown]);
        assert_eq!(config.path_template, "{repo}/{branch}/{sha7}.md");
    }

    #[test]
    fn accepts_every_known_placeholder() {
        assert!(validate_path_template("{repo}/{branch}/{date}-{sha}-{sha7}-{ticket}-{status}.md").is_ok());
        assert!(validate_path_template("reviews.md").is_ok());
    }

    #[test]
    fn rejects_bad_templates() {
        let cases = [
            ("", "empty"),
            ("{repo}/{sha7}.txt", ".md"),
            ("/abs/{sha7}.md", "relative"),
            ("C:/x/{sha7}.md", "relative"),
            ("a\\{sha7}.md", "relative"),
            ("{repo}/../{sha7}.md", "'..'"),
            ("{repo}//{sha7}.md", "empty"),
            ("./{sha7}.md", "'.'"),
            ("{repo}/{user}.md", "unknown placeholder {user}"),
            ("{repo}/{sha7.md", "without a matching '}'"),
            ("{repo}/sha7}.md", "without a matching '{'"),
            ("{repo}/{branch}/latest.md", "reserved"),
        ];
        for (template, expected) in cases {
            let err = validate_path_template(template).unwrap_err();
            assert!(err.contains(expected), "{template}: {err}");
        }
        assert!(validate_path_template(&format!("{}.md", "a".repeat(MAX_TEMPLATE_LEN))).is_err());
    }

    #[test]
    fn config_validation_names_the_field() {
        let bad = |f: fn(&mut OutputConfig)| {
            let mut c = OutputConfig::default();
            f(&mut c);
            c.validate().unwrap_err().field
        };
        assert_eq!(bad(|c| c.dir = "reviews".into()), "output.dir");
        assert_eq!(bad(|c| c.path_template = "x.txt".into()), "output.path_template");
        assert_eq!(bad(|c| c.formats.clear()), "output.formats");
        assert_eq!(bad(|c| c.keep_last = MAX_KEEP_LAST + 1), "output.keep_last");
        assert_eq!(bad(|c| c.rules_file = "../x.md".into()), "output.rules_file");
        assert_eq!(bad(|c| c.path_template = "{repo}/qgate-rules.md".into()), "output.rules_file");
        assert_eq!(bad(|c| c.rules_max = 0), "output.rules_max");
        assert_eq!(bad(|c| c.rules_max = MAX_RULES_MAX + 1), "output.rules_max");
        let disabled = OutputConfig { enabled: false, formats: vec![], keep_last: 0, ..OutputConfig::default() };
        assert!(disabled.validate().is_ok());
    }

    #[test]
    fn rules_file_must_be_a_plain_markdown_name() {
        let config = OutputConfig::default();
        assert!(config.rules_enabled);
        assert_eq!((config.rules_file.as_str(), config.rules_max), ("qgate-rules.md", 100));
        assert!(validate_rules_file("AGENT_rules-v2.md").is_ok());
        let cases = [
            ("", "1 to"),
            (".md", "ending with"),
            ("rules.txt", "ending with"),
            (".rules.md", "plain"),
            ("a/rules.md", "plain"),
            ("a\\rules.md", "plain"),
            ("ru les.md", "plain"),
            ("latest.md", "reserved"),
        ];
        for (name, expected) in cases {
            let err = validate_rules_file(name).unwrap_err();
            assert!(err.contains(expected), "{name}: {err}");
        }
        assert!(validate_rules_file(&format!("{}.md", "a".repeat(MAX_RULES_FILE_LEN))).is_err());
    }

    #[test]
    fn older_configs_without_rule_fields_get_the_defaults() {
        let config: OutputConfig = serde_json::from_value(serde_json::json!({"keep_last": 3})).unwrap();
        assert_eq!(config.rules_file, "qgate-rules.md");
        assert!(config.rules_enabled);
    }

    #[test]
    fn formats_serialize_lowercase() {
        assert_eq!(serde_json::to_value(OutputFormat::Markdown).unwrap(), "markdown");
        assert_eq!(serde_json::to_value(OutputFormat::Json).unwrap(), "json");
        assert_eq!(serde_json::to_value(OutputFormat::Html).unwrap(), "html");
        let parsed: Vec<OutputFormat> = serde_json::from_str(r#"["markdown","html"]"#).unwrap();
        assert_eq!(parsed, vec![OutputFormat::Markdown, OutputFormat::Html]);
        assert!(serde_json::from_str::<OutputFormat>(r#""pdf""#).is_err());
    }

    #[test]
    fn html_is_opt_in_and_valid() {
        assert!(!OutputConfig::default().formats.contains(&OutputFormat::Html));
        let html_only = OutputConfig { formats: vec![OutputFormat::Html], ..OutputConfig::default() };
        assert!(html_only.validate().is_ok());
    }
}
