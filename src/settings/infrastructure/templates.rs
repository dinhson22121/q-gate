pub const CONFIG_TEMPLATE: &str = r#"# qgate repository configuration.
# Every section and key is optional; the values below are the defaults.

[llm]
# Model used for reviews. Must be a full, pinned model id; aliases (opus, sonnet, haiku, default, best), -latest and [1m] are rejected.
model = "claude-opus-5-5"
# Reasoning effort: low | medium | high | xhigh | max
effort = "high"
# The `claude` binary is not set here: for security it can only be set in the server config (qgate.toml).
# Parallel review requests (1..=16).
concurrency = 4
# Second LLM pass that drops hallucinated or contradicted findings (never drops without grounded counter-evidence).
audit_findings = true

[gate]
# Findings at or above this severity block the gate: info | minor | major | critical
fail_on = "critical"
# Only these categories can block: security, correctness, concurrency, data_loss,
# performance, error_handling, maintainability, testing, architecture
categories = ["security", "correctness", "concurrency", "data_loss", "architecture"]
# Minimum confidence for a blocking finding: low | medium | high
min_confidence = "high"
# Fail when a Jira acceptance criterion is assessed as not met.
fail_on_unmet_criteria = true
# Fail when some files could not be reviewed (e.g. Claude unavailable or refusing).
fail_on_incomplete = true
# Criteria, tests or files qgate could not verify: strict | advisory.
# strict blocks on them (subject to the fail_on_* switches); advisory lists them as unverified without blocking.
unverifiable = "strict"

[report]
# Findings shown in reports (blocking or not).
min_severity = "minor"
min_confidence = "medium"

[review]
# Extra glob patterns to skip. Lockfiles, vendor/, node_modules/, dist/, build/, target/,
# minified and generated files are always skipped.
ignore = []
# Inline review guidelines, appended to the guideline files below.
guidelines = ""
# Repository files passed to the reviewer as guidelines (missing files are ignored).
guideline_files = ["CLAUDE.md", "AGENTS.md", "CONTRIBUTING.md", ".qgate/guidelines.md"]
# Files larger than this (KB) are not reviewed.
max_file_kb = 300

# Deterministic checks, run in the repository root without a shell.
# A failing required check fails the gate regardless of the model. Repeat the block per check.
# [[checks]]
# name = "unit"
# command = ["go", "test", "./..."]
# required = true
# timeout_secs = 600

[jira]
# Unset keys fall back to the server configuration (qgate.toml).
# The connection (base_url, provider) can only be set in the server config, for security.
# enabled = false
# Custom field holding the acceptance criteria, e.g. "customfield_10050" (empty: parse the description).
# ac_field = ""
# Only recognise issue keys of these projects, e.g. ["PROJ"] (empty: any project).
# key_pattern_projects = []

[watch]
# Review this repository in the background.
enabled = true
# Review a branch automatically when it is pushed (the legacy key review_on_commit still works).
review_on_push = true
"#;

pub const SERVER_CONFIG_TEMPLATE: &str = r#"# qgate server configuration (user level, not part of any repository).
# This file is only used to seed the settings database on first start; after that it is no longer read.
# Change settings on the Settings page of the dashboard (/settings).
# Every section and key is optional; the values below are the defaults.
# Environment overrides: QGATE_BIND, QGATE_REPOS_ROOT (comma-separated), QGATE_WEBHOOK_URL,
# QGATE_CLAUDE_BIN, QGATE_CLI_TIMEOUT_SECS, JIRA_BASE_URL, QGATE_JIRA_ENABLED, QGATE_OUTPUT_DIR. Secrets come from
# FIGMA_TOKEN, their *_FILE variants (Docker secrets) or the credential store - never from here.

[dashboard]
# Address the dashboard listens on. Use "0.0.0.0:8080" inside a container.
bind = "127.0.0.1:8080"
# Accepted Host header values (protects against DNS rebinding).
allowed_hosts = ["localhost", "127.0.0.1"]

[repos]
# Absolute directories scanned for git repositories when auto_scan is on.
roots = ["/repos"]
# Watch every git repository found under roots (true) or only the imported ones (false). Env: QGATE_REPOS_AUTO_SCAN.
auto_scan = false
# Directory levels searched below each root.
scan_depth = 3

[notify]
# Webhook receiving review results (empty: disabled).
webhook_url = ""

[watch]
# Seconds between checks for pushed branches.
poll_interval_secs = 2
# Reviews running at the same time across all repositories.
max_concurrent_reviews = 2
# Run the [[checks]] commands from each repo's .qgate.toml (tests, linters) on every review.
# They are repository-controlled commands: turn off if /repos contains code you do not trust.
run_repo_checks = true
# Git remote whose pushes are watched (refs/remotes/<remote>/*).
remote = "origin"
# Branch a pushed branch is compared against (merge-base); a push to it is never reviewed.
# Empty = auto-resolve <remote>/main then <remote>/master. Admin-only (QGATE_WATCH_BASE_BRANCH).
base_branch = ""

[quality]
# Best-effort quality tools (secret scan, dependency vulnerabilities, linters) on every review. Env: QGATE_QUALITY_ENABLED.
enabled = true

[regression]
# Re-run the repository's tests on the base commit to flag newly failing tests. Env: QGATE_REGRESSION_ENABLED.
enabled = true

[llm]
# Claude Code CLI (runs on your Claude subscription; authenticate with `claude login` or CLAUDE_CODE_OAUTH_TOKEN).
# Server-only (admin): the binary runs with your credentials, so repositories cannot change it.
# Path of the `claude` executable (looked up on PATH when it has no directory). Env: QGATE_CLAUDE_BIN.
claude_bin = "claude"
# Wall-clock limit of one `claude -p` run, in seconds. Env: QGATE_CLI_TIMEOUT_SECS.
timeout_secs = 600

[jira]
# Connection settings are server-only; a repository's .qgate.toml may only set
# enabled, ac_field and key_pattern_projects.
enabled = false
base_url = "https://yourcompany.atlassian.net"
# Atlassian Remote MCP, connected from the Settings page.
provider = "mcp_oauth"
ac_field = ""
key_pattern_projects = []

[agents]
# Multi-agent ticket review: ticket analysis, generated tests and architecture conformance.
enabled = true
model = "claude-opus-5-5"
# low | medium | high | xhigh | max
effort = "high"
# Sub-agents running at the same time for one review (1-8).
max_parallel = 3

[agents.ticket_analyst]
enabled = true
# Walk Sub-task -> Story -> Epic and merge their acceptance criteria and design links.
include_parent_story = true
# Pass the ticket description to reviewers as context (never gating).
use_description = true
# Re-fetch a ticket after this many minutes even if Jira's "updated" is unchanged (1-1440).
cache_ttl_minutes = 10
# Jira issue-link name that points to the QA ticket whose criteria are merged (empty: do not follow links).
qa_link_type = "is tested by"

[agents.tests]
# Generate and run tests for the acceptance criteria. Languages: go, java, python, rust
# (JavaScript/TypeScript comes later).
enabled = true
languages = ["go", "java", "python", "rust"]
# Scenarios per ticket (1-30), one agent each.
max_scenarios = 8
# Times an agent may fix its own test after a compile/setup error (0-6).
max_repair_rounds = 3
# Seconds per test run (10-3600).
timeout_secs = 300
# warn: shown on the dashboard | block: fails the gate. A failure only counts with complete evidence
# (criterion, scenario, test source, command, exit code, assertion output, code under test, commit,
# reproduce command); otherwise it is reported as inconclusive.
on_fail = "warn"

[agents.architecture]
enabled = true
# Rebuild a repository's cached rulebook after this many days even if no rule file changed (1-90).
rulebook_refresh_days = 7
# Inferred (not yet approved) rules are reported but never block.
use_inferred_rules = true
# warn | block
on_violation = "warn"

[figma]
# Design checks against Figma frames linked from the ticket's parent Story.
# Figma's cloud MCP server (mcp.figma.com) only accepts OAuth from catalog-listed clients, so qgate uses the
# REST API with a personal access token (FIGMA_TOKEN / FIGMA_TOKEN_FILE, or the settings page).
enabled = false
# rest | mcp
provider = "rest"
api_base_url = "https://api.figma.com"
# Only used with provider = "mcp".
mcp_url = "https://mcp.figma.com/mcp"
# copy: text missing from the code/i18n | component: other component than the design system's |
# token: hard-coded colour/spacing/font where the design binds a variable | states: unhandled variants.
checks = ["copy", "component", "token", "states"]
# Checks that fail the gate (copy, component, token; "states" is judged by the model and cannot block).
block_on = []

[output]
# Review files written after each scan, to hand to your coding agent (Claude Code, Cursor...).
enabled = true
# Absolute folder the files go to. In Docker it is /reviews, mounted from REVIEWS_PATH on your machine
# (env override: QGATE_OUTPUT_DIR; changing it needs a restart).
dir = "/reviews"
# File path below dir. Placeholders: {repo} {branch} {sha7} {sha} {date} {ticket} {status}.
# Must be relative and end with .md. Values are sanitised (feature/ABC-1 -> feature-ABC-1).
path_template = "{repo}/{branch}/{sha7}.md"
# Also write {repo}/{branch}/latest.md (and latest.json / latest.html when enabled) - point your coding agent at it.
write_latest = true
# markdown | json | html. Markdown is the coding agent's input; html is opt-in: a self-contained report
# (all sections, escaped, prints cleanly) to attach to Jira as evidence or save as PDF.
formats = ["markdown"]
# Also write a file when the gate passed (latest.md is always kept current).
include_passing = true
# Generated files kept per folder (0-1000, 0 = keep all); older generated files are deleted, nothing else.
keep_last = 20
# Turn every finding into a rule for coding agents: merged into {repo}/<rules_file> after each scan
# (deduplicated, with hit counts; state in .<name>.json next to it). Reference it from CLAUDE.md / AGENTS.md.
rules_enabled = true
# Plain .md file name in the repository's folder.
rules_file = "qgate-rules.md"
# Rules kept per repository (1-1000); the least severe, least recently seen are dropped first.
rules_max = 100
"#;
