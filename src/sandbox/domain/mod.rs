//! Pure rules for running untrusted commands: the request/result model, request limits, the
//! environment allow-list and the bounded output tail.

mod output;
mod request;

pub use output::OutputTail;
pub use request::{
    ExecFailure, ExecRequest, ExecResult, FailureKind, INHERITED_ENV_VARS, MAX_OUTPUT_BYTES, MAX_TIMEOUT_MS, is_inside,
    validate,
};
