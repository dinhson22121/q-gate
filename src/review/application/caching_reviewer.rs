use crate::cache::application::{CacheStore, CacheTags, ContentKey, ResultCache, digest};
use crate::review::application::ports::{
    AssessmentOutput, CodeReviewer, RequirementsInput, ReviewInput, ReviewerError, ReviewerOutput,
};
use crate::review::domain::change_set::FileChange;
use crate::review::domain::finding::Finding;

use std::sync::Arc;

const KIND: &str = "file_review";
const SCHEMA_VERSION: u32 = 2;

pub struct CachingCodeReviewer<R> {
    inner: R,
    store: CacheStore,
    tags: CacheTags,
}

impl<R: CodeReviewer> CachingCodeReviewer<R> {
    pub fn new(inner: R, cache: Arc<dyn ResultCache>, tags: CacheTags, no_cache: bool) -> Self {
        Self { inner, store: CacheStore::new(cache, no_cache), tags }
    }

    fn batch_key(&self, input: &ReviewInput<'_>) -> ContentKey {
        let key = self.tags.apply(ContentKey::new(KIND, SCHEMA_VERSION));
        let key = key.with_input("guidelines", digest([input.guidelines]));
        let callers: Vec<String> = input
            .callers
            .iter()
            .map(|c| format!("{}|{}|{}|{}|{}", c.symbol, c.defined_in, c.file, c.line, c.snippet))
            .collect();
        let key = key.with_input("callers", digest(callers.iter().map(String::as_str)));
        key.with_inputs(input.files.iter().map(|f| (format!("file:{}", f.path), file_digest(f))))
    }
}

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
