//! Testing bounded context: turns a test scenario into ONE generated test, runs it in the review's
//! writable snapshot and reports a verdict that qgate (not the model) decides from the real run.
//!
//! `domain` holds the pure rules (language profiles, path policy, run classification, verdict and
//! evidence assembly); `application` the tool handler and the [`TestAgent`] use case behind ports;
//! `infrastructure` the snapshot file system, the process runner and the Claude adapter.
//!
//! [`TestAgent`]: crate::review::application::agent_ports::TestAgent

pub mod application;
pub mod domain;
pub mod infrastructure;

pub use application::agent::{ScenarioTestAgent, TestAgentSettings};
pub use application::caching::CachingTestAgent;
pub use domain::language::TestLanguage;
pub use infrastructure::ClaudeTestAgent;
