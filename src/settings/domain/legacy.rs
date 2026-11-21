use serde::Deserialize;

#[derive(Debug, Clone, Default)]
pub(super) struct LegacyField;

impl PartialEq for LegacyField {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for LegacyField {}

impl<'de> Deserialize<'de> for LegacyField {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(deserializer).map(|_| Self)
    }
}
