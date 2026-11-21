//! Repository configuration (`.qgate.toml`): model, gate policy, review scope, checks, Jira, watch.

use serde::{Deserialize, Serialize};

use super::jira::{JiraConfig, JiraOverride, require_command};
use super::validation::{ValidationError, ValidationResult, require_non_empty, require_range};
use crate::review::application::{Category, CheckSpec, Confidence, GatePolicy, Severity};

const DEFAULT_MODEL: &str = "claude-opus-5-5";
const DEFAULT_MAX_TOKENS: u32 = 16_000;
const MIN_MAX_TOKENS: u32 = 1024;
/// Upper bound accepted for `max_tokens`; prevents typos like an extra zero burning budget.
const MAX_MAX_TOKENS: u32 = 1_000_000;
const DEFAULT_CONCURRENCY: usize = 4;
const MIN_CONCURRENCY: usize = 1;
const MAX_CONCURRENCY: usize = 16;
const DEFAULT_MAX_FILE_KB: u64 = 300;
const MAX_FILE_KB_LIMIT: u64 = 10 * 1024;
const BYTES_PER_KB: u64 = 1024;
const DEFAULT_CHECK_TIMEOUT_SECS: u64 = 600;
const MAX_CHECK_TIMEOUT_SECS: u64 = 6 * 60 * 60;
const DEFAULT_GUIDELINE_FILES: [&str; 4] = ["CLAUDE.md", "AGENTS.md", "CONTRIBUTING.md", ".qgate/guidelines.md"];

/// Paths never worth reviewing: lockfiles, vendored and build output, minified and generated code.
pub const BUILTIN_IGNORE_PATTERNS: &[&str] = &[
    "**/package-lock.json",
    "**/npm-shrinkwrap.json",
    "**/yarn.lock",
    "**/pnpm-lock.yaml",
    "**/bun.lockb",
    "**/Cargo.lock",
    "**/go.sum",
    "**/poetry.lock",
    "**/Pipfile.lock",
    "**/uv.lock",
    "**/composer.lock",
    "**/Gemfile.lock",
    "**/gradle.lockfile",
    "**/packages.lock.json",
    "**/vendor/**",
    "**/node_modules/**",
    "**/dist/**",
    "**/build/**",
    "**/target/**",
    "**/*.min.js",
    "**/*.min.css",
    "**/*.map",
    "**/*.pb.go",
    "**/*.pb.gw.go",
    "**/*_pb2.py",
    "**/*_pb2_grpc.py",
    "**/*.pb.h",
    "**/*.pb.cc",
    "**/*.pb.ts",
    "**/*_pb.js",
    "**/*_pb.d.ts",
    "**/*.generated.*",
    "**/*.snap",
];

/// Reasoning effort requested from the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    #[default]
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// Repository-level `[llm]` section. The endpoint (`base_url`) is deliberately absent: it decides where
/// the API key and the code are sent, so only the server config may set it (see `ServerLlmConfig`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LlmConfig {
    pub model: String,
    pub effort: Effort,
    pub max_tokens: u32,
    /// Retry with fallback models when the primary one is overloaded.
    pub fallbacks: bool,
    /// Parallel review requests.
    pub concurrency: usize,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            model: DEFAULT_MODEL.into(),
            effort: Effort::default(),
            max_tokens: DEFAULT_MAX_TOKENS,
            fallbacks: true,
            concurrency: DEFAULT_CONCURRENCY,
        }
    }
}

impl LlmConfig {
    fn validate(&self) -> ValidationResult {
        require_non_empty("llm.model", &self.model)?;
        require_range("llm.max_tokens", self.max_tokens, MIN_MAX_TOKENS, MAX_MAX_TOKENS)?;
        require_range("llm.concurrency", self.concurrency, MIN_CONCURRENCY, MAX_CONCURRENCY)
    }
}

/// What blocks the gate (mirrors [`GatePolicy`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GateConfig {
    pub fail_on: Severity,
    pub categories: Vec<Category>,
    pub min_confidence: Confidence,
    pub fail_on_unmet_criteria: bool,
    /// Fail when some files could not be reviewed (e.g. Claude unavailable or refusing).
    pub fail_on_incomplete: bool,
}

impl Default for GateConfig {
    fn default() -> Self {
        let policy = GatePolicy::default();
        Self {
            fail_on: policy.fail_on,
            categories: policy.categories,
            min_confidence: policy.min_confidence,
            fail_on_unmet_criteria: policy.fail_on_unmet_criteria,
            fail_on_incomplete: policy.fail_on_incomplete,
        }
    }
}

/// What is shown in reports (non-blocking findings included).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReportConfig {
    pub min_severity: Severity,
    pub min_confidence: Confidence,
}

impl Default for ReportConfig {
    fn default() -> Self {
        Self { min_severity: Severity::Minor, min_confidence: Confidence::Medium }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReviewConfig {
    /// Extra glob patterns to skip, added to [`BUILTIN_IGNORE_PATTERNS`].
    pub ignore: Vec<String>,
    /// Inline guidelines appended to the guideline files.
    pub guidelines: String,
    /// Repository files whose content is passed to the reviewer as guidelines.
    pub guideline_files: Vec<String>,
    /// Files larger than this are skipped.
    pub max_file_kb: u64,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            ignore: Vec::new(),
            guidelines: String::new(),
            guideline_files: DEFAULT_GUIDELINE_FILES.iter().map(|s| s.to_string()).collect(),
            max_file_kb: DEFAULT_MAX_FILE_KB,
        }
    }
}

impl ReviewConfig {
    fn validate(&self) -> ValidationResult {
        require_range("review.max_file_kb", self.max_file_kb, 1, MAX_FILE_KB_LIMIT)?;
        if self.ignore.iter().any(|p| p.trim().is_empty()) {
            return Err(ValidationError::new("review.ignore", "patterns must not be empty"));
        }
        Ok(())
    }
}

/// A deterministic check (`[[checks]]`), executed without a shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CheckConfig {
    pub name: String,
    pub command: Vec<String>,
    pub required: bool,
    pub timeout_secs: u64,
}

impl Default for CheckConfig {
    fn default() -> Self {
        Self { name: String::new(), command: Vec::new(), required: true, timeout_secs: DEFAULT_CHECK_TIMEOUT_SECS }
    }
}

impl CheckConfig {
    fn validate(&self, index: usize) -> ValidationResult {
        let field = |name: &str| format!("checks[{index}].{name}");
        require_non_empty(&field("name"), &self.name)?;
        require_command(&field("command"), &self.command)?;
        require_range(&field("timeout_secs"), self.timeout_secs, 1, MAX_CHECK_TIMEOUT_SECS)
    }

    fn to_spec(&self) -> CheckSpec {
        CheckSpec {
            name: self.name.clone(),
            command: self.command.clone(),
            required: self.required,
            timeout_secs: self.timeout_secs,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WatchConfig {
    pub enabled: bool,
    /// Review this branch when it is pushed. The old key `review_on_commit` is still accepted
    /// (the trigger is now a push, not a local commit).
    #[serde(alias = "review_on_commit")]
    pub review_on_push: bool,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self { enabled: true, review_on_push: true }
    }
}

/// Contents of a repository's `.qgate.toml`. Every section is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub llm: LlmConfig,
    pub gate: GateConfig,
    pub report: ReportConfig,
    pub review: ReviewConfig,
    pub checks: Vec<CheckConfig>,
    /// Per-repository Jira settings; unset fields fall back to the server defaults.
    pub jira: JiraOverride,
    pub watch: WatchConfig,
}

impl Config {
    /// Rejects out-of-range or malformed values with the offending field's path.
    pub fn validate(&self) -> ValidationResult {
        self.llm.validate()?;
        self.review.validate()?;
        for (index, check) in self.checks.iter().enumerate() {
            check.validate(index)?;
        }
        self.validate_unique_check_names()?;
        self.jira.validate("jira")
    }

    fn validate_unique_check_names(&self) -> ValidationResult {
        for (index, check) in self.checks.iter().enumerate() {
            if self.checks[..index].iter().any(|earlier| earlier.name == check.name) {
                return Err(ValidationError::new(
                    format!("checks[{index}].name"),
                    format!("duplicate check name '{}'", check.name),
                ));
            }
        }
        Ok(())
    }

    pub fn gate_policy(&self) -> GatePolicy {
        GatePolicy {
            fail_on: self.gate.fail_on,
            categories: self.gate.categories.clone(),
            min_confidence: self.gate.min_confidence,
            fail_on_unmet_criteria: self.gate.fail_on_unmet_criteria,
            fail_on_incomplete: self.gate.fail_on_incomplete,
            // Agent policies are server-level; the CLI fills them from `ServerConfig`.
            ..GatePolicy::default()
        }
    }

    pub fn check_specs(&self) -> Vec<CheckSpec> {
        self.checks.iter().map(CheckConfig::to_spec).collect()
    }

    /// Built-in ignore patterns followed by the repository's own, without duplicates.
    pub fn ignore_patterns(&self) -> Vec<String> {
        let mut patterns: Vec<String> = BUILTIN_IGNORE_PATTERNS.iter().map(|p| p.to_string()).collect();
        for pattern in &self.review.ignore {
            if !patterns.contains(pattern) {
                patterns.push(pattern.clone());
            }
        }
        patterns
    }

    pub fn max_file_bytes(&self) -> usize {
        usize::try_from(self.review.max_file_kb.saturating_mul(BYTES_PER_KB)).unwrap_or(usize::MAX)
    }

    /// Effective Jira settings: this repository's overrides on top of `defaults` (the server config).
    pub fn jira(&self, defaults: &JiraConfig) -> JiraConfig {
        self.jira.apply_to(defaults)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str) -> CheckConfig {
        CheckConfig { name: name.into(), command: vec!["go".into(), "test".into()], ..CheckConfig::default() }
    }

    #[test]
    fn defaults_are_valid_and_match_documented_values() {
        let config = Config::default();
        assert!(config.validate().is_ok());
        assert_eq!(config.llm.model, "claude-opus-5-5");
        assert_eq!(config.llm.effort, Effort::High);
        assert_eq!(config.llm.max_tokens, 16_000);
        assert_eq!(config.report.min_severity, Severity::Minor);
        assert_eq!(config.review.guideline_files.len(), 4);
        assert!(config.watch.enabled && config.watch.review_on_push);
        assert_eq!(config.gate_policy(), GatePolicy::default());
    }

    #[test]
    fn llm_values_are_range_checked() {
        let cases = [
            (LlmConfig { model: " ".into(), ..LlmConfig::default() }, "llm.model"),
            (LlmConfig { max_tokens: 1023, ..LlmConfig::default() }, "llm.max_tokens"),
            (LlmConfig { concurrency: 0, ..LlmConfig::default() }, "llm.concurrency"),
            (LlmConfig { concurrency: 17, ..LlmConfig::default() }, "llm.concurrency"),
        ];
        for (llm, field) in cases {
            let config = Config { llm, ..Config::default() };
            assert_eq!(config.validate().unwrap_err().field, field);
        }
    }

    #[test]
    fn review_values_are_checked() {
        let config = Config { review: ReviewConfig { max_file_kb: 0, ..ReviewConfig::default() }, ..Config::default() };
        assert_eq!(config.validate().unwrap_err().field, "review.max_file_kb");
        let config = Config {
            review: ReviewConfig { ignore: vec![" ".into()], ..ReviewConfig::default() },
            ..Config::default()
        };
        assert_eq!(config.validate().unwrap_err().field, "review.ignore");
    }

    #[test]
    fn checks_are_validated() {
        let cases = [
            (CheckConfig { name: String::new(), ..check("a") }, "checks[0].name"),
            (CheckConfig { command: vec![], ..check("a") }, "checks[0].command"),
            (CheckConfig { command: vec![String::new()], ..check("a") }, "checks[0].command"),
            (CheckConfig { timeout_secs: 0, ..check("a") }, "checks[0].timeout_secs"),
        ];
        for (c, field) in cases {
            let config = Config { checks: vec![c], ..Config::default() };
            assert_eq!(config.validate().unwrap_err().field, field);
        }
        let duplicate = Config { checks: vec![check("unit"), check("lint"), check("unit")], ..Config::default() };
        let err = duplicate.validate().unwrap_err();
        assert_eq!(err.field, "checks[2].name");
        assert!(err.message.contains("duplicate"));
    }

    #[test]
    fn check_specs_mirror_config() {
        let config = Config {
            checks: vec![CheckConfig { required: false, timeout_secs: 5, ..check("unit") }],
            ..Config::default()
        };
        let specs = config.check_specs();
        assert_eq!(
            specs,
            vec![CheckSpec {
                name: "unit".into(),
                command: vec!["go".into(), "test".into()],
                required: false,
                timeout_secs: 5
            }]
        );
    }

    #[test]
    fn ignore_patterns_merge_builtins_without_duplicates() {
        let review = ReviewConfig { ignore: vec!["docs/**".into(), "**/yarn.lock".into()], ..ReviewConfig::default() };
        let patterns = Config { review, ..Config::default() }.ignore_patterns();
        assert_eq!(patterns.len(), BUILTIN_IGNORE_PATTERNS.len() + 1);
        assert_eq!(patterns.last().map(String::as_str), Some("docs/**"));
    }

    #[test]
    fn max_file_bytes_converts_kilobytes() {
        assert_eq!(Config::default().max_file_bytes(), 300 * 1024);
    }

    #[test]
    fn jira_merges_repo_overrides_onto_server_defaults() {
        let server = JiraConfig { email: "bot@acme.com".into(), ..JiraConfig::default() };
        let config =
            Config { jira: JiraOverride { enabled: Some(true), ..JiraOverride::default() }, ..Config::default() };
        let merged = config.jira(&server);
        assert!(merged.enabled);
        assert_eq!(merged.email, "bot@acme.com");
        let bad = Config {
            jira: JiraOverride { ac_field: Some("summary".into()), ..JiraOverride::default() },
            ..Config::default()
        };
        assert_eq!(bad.validate().unwrap_err().field, "jira.ac_field");
    }

    #[test]
    fn watch_review_on_push_accepts_the_legacy_key() {
        let legacy: Config = toml::from_str("[watch]\nreview_on_commit = false\n").unwrap();
        assert!(!legacy.watch.review_on_push);
        let current: Config = toml::from_str("[watch]\nreview_on_push = false\n").unwrap();
        assert!(!current.watch.review_on_push);
    }

    #[test]
    fn effort_names_are_lowercase() {
        assert_eq!(Effort::Xhigh.as_str(), "xhigh");
        assert_eq!(Effort::Low.as_str(), "low");
        assert_eq!(Effort::Medium.as_str(), "medium");
        assert_eq!(Effort::Max.as_str(), "max");
        assert_eq!(Effort::High.as_str(), "high");
    }
}
