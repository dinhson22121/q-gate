use crate::review::domain::language::Language;
use crate::settings::application::TestLanguage;

pub fn languages_from_settings(configured: &[TestLanguage]) -> Vec<Language> {
    configured
        .iter()
        .map(|language| match language {
            TestLanguage::Go => Language::Go,
            TestLanguage::Java => Language::Java,
            TestLanguage::Python => Language::Python,
            TestLanguage::Rust => Language::Rust,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_configured_language_maps_to_its_review_language() {
        let all = [TestLanguage::Go, TestLanguage::Java, TestLanguage::Python, TestLanguage::Rust];
        let expected = vec![Language::Go, Language::Java, Language::Python, Language::Rust];
        assert_eq!(languages_from_settings(&all), expected);
    }
}
