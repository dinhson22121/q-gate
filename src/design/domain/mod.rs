pub mod checks;
mod component_check;
mod copy_check;
pub mod link;
pub mod spec;
mod text;
mod token_check;
pub mod ui;

#[cfg(test)]
pub(crate) mod fixtures;

pub use checks::{ALL_CHECKS, DesignCheckKind, detect_mismatches};
pub use link::{FigmaLink, LinkError};
pub use spec::{ComponentRef, DesignSpec, TokenKind, TokenRef};
