//! Decorator that reuses a batch's findings when none of the code the model saw has changed.
//!
//! A batch is reviewed as a whole (findings can depend on the other files in it), so the key holds a
//! content digest of every file in the batch (path, status, diff and new content), the guidelines
//! and the model/prompt tags. Any change to any of them is a miss. Errors are never cached; the
//! requirements assessment depends on the ticket and always goes to the inner reviewer.

use crate::cache::application::{CacheStore, CacheTags, ContentKey, ResultCache, digest};
use crate::review::application::ports::{
    AssessmentOutput, CodeReviewer, RequirementsInput, ReviewInput, ReviewerError, ReviewerOutput,
};
use crate::review::domain::change_set::FileChange;
use crate::review::domain::finding::Finding;

use std::sync::Arc;

const KIND: &str = "file_review";
const SCHEMA_VERSION: u32 = 1;

/// A [`CodeReviewer`] that consults the cache before calling `inner`.
pub struct CachingCodeReviewer<R> {
    inner: R,
    store: CacheStore,
    tags: CacheTags,
}

impl<R: CodeReviewer> CachingCodeReviewer<R> {
    /// `no_cache` bypasses every lookup and store (`qgate: no-cache` trailer / dashboard button).
    pub fn new(inner: R, cache: Arc<dyn ResultCache>, tags: CacheTags, no_cache: bool) -> Self {
        Self { inner, store: CacheStore::new(cache, no_cache), tags }
    }

    fn batch_key(&self, input: &ReviewInput<'_>) -> ContentKey {
        let key = self.tags.apply(ContentKey::new(KIND, SCHEMA_VERSION));
        let key = key.with_input("guidelines", digest([input.guidelines]));
        key.with_inputs(input.files.iter().map(|f| (format!("file:{}", f.path), file_digest(f))))
    }
}

/// Digest of everything the model is shown about one file.
fn file_digest(file: &FileChange) -> String {
    let hunks = format!("{:?}", file.hunks);
    let status = format!("{:?}", file.status);
    digest([file.path.as_str(), status.as_str(), hunks.as_str(), file.new_content.as_deref().unwrap_or("")])
}

impl<R: CodeReviewer + Sync> CodeReviewer for CachingCodeReviewer<R> {
    async fn review(&self, input: ReviewInput<'_>) -> Result<ReviewerOutput, ReviewerError> {
        let key = self.batch_key(&input);
        if let Some(findings) = self.store.get::<Vec<Finding>>(&key).await {
            return Ok(ReviewerOutput { findings, ..ReviewerOutput::default() });
        }
        let output = self.inner.review(input).await?;
        self.store.put(&key, &output.findings).await;
        Ok(output)
    }

    fn assess_requirements(
        &self,
        input: RequirementsInput<'_>,
    ) -> impl Future<Output = Result<AssessmentOutput, ReviewerError>> + Send {
        self.inner.assess_requirements(input)
    }
}

#[cfg(test)]
#[path = "caching_reviewer_tests.rs"]
mod tests;
