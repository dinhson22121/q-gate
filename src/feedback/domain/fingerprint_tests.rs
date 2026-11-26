use super::*;

fn facts(evidence: &str) -> FindingFacts {
    FindingFacts {
        file: "src/a.rs".into(),
        category: "security".into(),
        source: "llm".into(),
        evidence: evidence.into(),
    }
}

#[test]
fn normalizes_whitespace_and_blank_lines() {
    assert_eq!(normalize_evidence("  let   x = 1;  \n\n\t foo(  )\r\n   \n"), "let x = 1;\nfoo( )");
}

#[test]
fn ignores_formatting_differences_in_evidence() {
    assert_eq!(fingerprint(&facts("a  b\n\n c")), fingerprint(&facts(" a b \n c ")));
}

#[test]
fn differs_when_any_component_changes() {
    let base = fingerprint(&facts("x"));
    assert_ne!(base, fingerprint(&facts("y")));
    assert_ne!(base, fingerprint(&FindingFacts { file: "b".into(), ..facts("x") }));
    assert_ne!(base, fingerprint(&FindingFacts { category: "performance".into(), ..facts("x") }));
    assert_ne!(base, fingerprint(&FindingFacts { source: "tool:gitleaks".into(), ..facts("x") }));
}

#[test]
fn separator_prevents_component_collisions() {
    let a = FindingFacts { file: "ab".into(), category: "c".into(), ..facts("x") };
    let b = FindingFacts { file: "a".into(), category: "bc".into(), ..facts("x") };
    assert_ne!(fingerprint(&a), fingerprint(&b));
}

#[test]
fn produces_lowercase_hex_sha256() {
    let value = fingerprint(&facts("x"));
    assert!(is_fingerprint(&value));
    assert_eq!(value, fingerprint(&facts("x")));
}

#[test]
fn validates_fingerprint_shape() {
    assert!(!is_fingerprint(""));
    assert!(!is_fingerprint(&"A".repeat(64)));
    assert!(!is_fingerprint(&"a".repeat(63)));
    assert!(!is_fingerprint(&"g".repeat(64)));
    assert!(is_fingerprint(&"0f".repeat(32)));
}
