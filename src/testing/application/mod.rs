//! Use case "test one scenario" and the tool handler the agent works through.

pub mod agent;
pub mod caching;
pub mod handler;
pub mod ports;
mod prompt;

pub use prompt::prompt_fingerprint;

#[cfg(test)]
pub(crate) mod fakes;
