//! Content-addressed cache key: a namespace, a schema version and the content hashes of the
//! inputs a cached result was derived from. Two keys with the same inputs always share a
//! [`ContentKey::fingerprint`], so a result can be reused only when every input is unchanged.

use sha2::{Digest, Sha256};

/// Domain-separation tag; bump it to invalidate every fingerprint of a previous layout.
const FINGERPRINT_TAG: &[u8] = b"qgate.cache.content-key.v1";

/// A cache key built from the hashes of the inputs that produced a result.
///
/// `kind` namespaces the key (and is stored as the cache row's namespace column); `schema_version`
/// lets a producer invalidate its own entries when its output format changes. `inputs` are
/// `(label, content-hash)` pairs (for example `("file:src/a.rs", "<blob-oid>")`); their order does
/// not affect the fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentKey {
    kind: String,
    schema_version: u32,
    inputs: Vec<(String, String)>,
}

impl ContentKey {
    /// A key in namespace `kind` for producer schema version `schema_version`, with no inputs yet.
    pub fn new(kind: impl Into<String>, schema_version: u32) -> Self {
        Self { kind: kind.into(), schema_version, inputs: Vec::new() }
    }

    /// Adds one `(label, content-hash)` input. Builder style; order is irrelevant to the fingerprint.
    #[must_use]
    pub fn with_input(mut self, label: impl Into<String>, content_hash: impl Into<String>) -> Self {
        self.inputs.push((label.into(), content_hash.into()));
        self
    }

    /// Adds every `(label, content-hash)` pair from `inputs`.
    #[must_use]
    pub fn with_inputs<L, H>(mut self, inputs: impl IntoIterator<Item = (L, H)>) -> Self
    where
        L: Into<String>,
        H: Into<String>,
    {
        self.inputs.extend(inputs.into_iter().map(|(l, h)| (l.into(), h.into())));
        self
    }

    /// The namespace, stored as the cache row's `kind` column.
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// The producer schema version baked into the fingerprint.
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Stable, lowercase-hex SHA-256 over the kind, schema version and the inputs sorted by
    /// `(label, hash)`. Each field is length-prefixed so no concatenation can collide with another.
    pub fn fingerprint(&self) -> String {
        let mut sorted = self.inputs.clone();
        sorted.sort();
        let mut hasher = Sha256::new();
        hasher.update(FINGERPRINT_TAG);
        write_field(&mut hasher, self.kind.as_bytes());
        hasher.update(self.schema_version.to_be_bytes());
        hasher.update((sorted.len() as u64).to_be_bytes());
        for (label, hash) in &sorted {
            write_field(&mut hasher, label.as_bytes());
            write_field(&mut hasher, hash.as_bytes());
        }
        let digest = hasher.finalize();
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

/// Feeds `bytes` into `hasher` prefixed by its length, so adjacent fields cannot be confused.
fn write_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> ContentKey {
        ContentKey::new("review.findings", 1).with_input("file:src/a.rs", "oid-a").with_input("file:src/b.rs", "oid-b")
    }

    #[test]
    fn same_inputs_yield_the_same_fingerprint() {
        assert_eq!(key().fingerprint(), key().fingerprint());
    }

    #[test]
    fn fingerprint_is_independent_of_input_order() {
        let reversed = ContentKey::new("review.findings", 1)
            .with_input("file:src/b.rs", "oid-b")
            .with_input("file:src/a.rs", "oid-a");
        assert_eq!(key().fingerprint(), reversed.fingerprint());
    }

    #[test]
    fn with_inputs_matches_repeated_with_input() {
        let built =
            ContentKey::new("review.findings", 1).with_inputs([("file:src/a.rs", "oid-a"), ("file:src/b.rs", "oid-b")]);
        assert_eq!(built.fingerprint(), key().fingerprint());
    }

    #[test]
    fn a_changed_input_hash_changes_the_fingerprint() {
        let changed = ContentKey::new("review.findings", 1)
            .with_input("file:src/a.rs", "oid-a")
            .with_input("file:src/b.rs", "oid-b2");
        assert_ne!(key().fingerprint(), changed.fingerprint());
    }

    #[test]
    fn kind_and_schema_version_are_part_of_the_fingerprint() {
        let other_kind = ContentKey::new("tests.scenarios", 1)
            .with_input("file:src/a.rs", "oid-a")
            .with_input("file:src/b.rs", "oid-b");
        let other_version = ContentKey::new("review.findings", 2)
            .with_input("file:src/a.rs", "oid-a")
            .with_input("file:src/b.rs", "oid-b");
        assert_ne!(key().fingerprint(), other_kind.fingerprint());
        assert_ne!(key().fingerprint(), other_version.fingerprint());
    }

    #[test]
    fn length_prefix_prevents_boundary_collisions() {
        let joined = ContentKey::new("review.findings", 1).with_input("file:src/a.rsoid-a", "");
        assert_ne!(
            joined.fingerprint(),
            ContentKey::new("review.findings", 1).with_input("file:src/a.rs", "oid-a").fingerprint()
        );
    }

    #[test]
    fn fingerprint_is_hex_sha256() {
        let fingerprint = key().fingerprint();
        assert_eq!(fingerprint.len(), 64);
        assert!(fingerprint.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn accessors_expose_kind_and_schema_version() {
        assert_eq!(key().kind(), "review.findings");
        assert_eq!(key().schema_version(), 1);
    }
}
