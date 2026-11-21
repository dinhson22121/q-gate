mod run_confined;

use futures::future::BoxFuture;

pub use crate::sandbox::domain::{ExecFailure, ExecRequest, ExecResult, FailureKind, MAX_OUTPUT_BYTES};
pub use run_confined::RunConfined;

pub trait ExecBackend: Send + Sync {
    fn execute(&self, request: ExecRequest) -> BoxFuture<'_, ExecResult>;
}

#[cfg(test)]
pub(crate) mod fakes {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    pub struct RecordingBackend {
        pub requests: Mutex<Vec<ExecRequest>>,
        pub result: ExecResult,
    }

    impl RecordingBackend {
        pub fn answering(result: ExecResult) -> Self {
            Self { requests: Mutex::default(), result }
        }
    }

    impl ExecBackend for RecordingBackend {
        fn execute(&self, request: ExecRequest) -> BoxFuture<'_, ExecResult> {
            self.requests.lock().unwrap().push(request);
            let result = self.result.clone();
            Box::pin(async move { result })
        }
    }
}
