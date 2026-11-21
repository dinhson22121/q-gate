//! Pure hand-off model: the review document for coding agents, its Markdown/JSON rendering and file paths.

pub mod document;
pub mod html;
pub mod json;
pub mod markdown;
pub mod path_template;
pub mod rules;
pub mod rules_markdown;
mod tasks;
pub mod text;

#[cfg(test)]
pub(crate) mod fixtures;
#[cfg(test)]
mod tests;

pub use document::{
    BlockingRules, CodeBlock, CriterionRow, DocumentMeta, Fact, HandoffContext, HandoffDocument, HandoffInput, RunMeta,
    Task, TaskKind, TicketInfo, Verdict,
};
pub use html::render_html;
pub use json::{JSON_SCHEMA, render_json};
pub use markdown::render_markdown;
pub use path_template::{PathTemplate, PathValues, Placeholder, RenderedPath, StemMatcher, TemplateError};
pub use rules::{RuleBook, RuleCandidate, RulesPaths, Sighting};
pub use rules_markdown::render_rules_markdown;
