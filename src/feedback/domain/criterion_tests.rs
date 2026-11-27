use super::*;

#[test]
fn verdict_round_trips_through_its_string_form() {
    for verdict in [CriterionVerdict::VerifiedMet, CriterionVerdict::NotApplicable, CriterionVerdict::Clarified] {
        assert_eq!(CriterionVerdict::parse(verdict.as_str()), Some(verdict));
    }
    assert_eq!(CriterionVerdict::parse("maybe"), None);
}

#[test]
fn verdict_serialises_as_snake_case() {
    assert_eq!(serde_json::to_string(&CriterionVerdict::VerifiedMet).unwrap(), "\"verified_met\"");
}

#[test]
fn hash_ignores_whitespace_but_not_wording() {
    assert_eq!(criterion_text_hash("Total  includes\n tax "), criterion_text_hash("Total includes tax"));
    assert_ne!(criterion_text_hash("Total includes tax"), criterion_text_hash("Total excludes tax"));
    assert_eq!(criterion_text_hash("x").len(), 64);
}

#[test]
fn clarification_requires_a_note() {
    assert!(validate_note(CriterionVerdict::Clarified, "  ").is_err());
    assert_eq!(validate_note(CriterionVerdict::Clarified, " CSV only ").unwrap(), "CSV only");
}

#[test]
fn other_verdicts_allow_an_empty_note() {
    assert_eq!(validate_note(CriterionVerdict::VerifiedMet, "").unwrap(), "");
    assert_eq!(validate_note(CriterionVerdict::NotApplicable, "n/a").unwrap(), "n/a");
}

#[test]
fn note_length_is_capped() {
    let at_limit = "a".repeat(MAX_NOTE_CHARS);
    assert!(validate_note(CriterionVerdict::VerifiedMet, &at_limit).is_ok());
    assert!(validate_note(CriterionVerdict::VerifiedMet, &format!("{at_limit}a")).is_err());
}
