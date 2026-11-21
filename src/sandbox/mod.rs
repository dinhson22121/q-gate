//! Sandbox bounded context: running untrusted, repository-controlled commands (repo checks,
//! model-written tests, the build scripts they trigger).
//!
//! Other contexts depend only on [`application::ExecBackend`]. In Docker the commands run in a
//! separate runner container (`qgate runner`) that has no secrets, no qgate database/config/review
//! mounts and no route to the dashboard; natively they run as children of qgate.

pub mod application;
pub mod domain;
pub mod infrastructure;
