use roxmltree::{Document, Node};

use crate::impact::domain::{TestOutcome, TestResult, TestRun};

const OUTPUT_TAIL_CHARS: usize = 4000;

pub fn parse_junit(xml: &str, into: &mut TestRun) -> Result<(), String> {
    let document = Document::parse(xml).map_err(|e| format!("invalid JUnit XML: {e}"))?;
    for case in document.descendants().filter(|n| n.has_tag_name("testcase")) {
        let Some(name) = case.attribute("name").map(str::trim).filter(|n| !n.is_empty()) else { continue };
        let class = case.attribute("classname").or_else(|| suite_name(case)).unwrap_or_default().trim();
        into.insert(class, name, TestResult { outcome: outcome(case), output_tail: output(case) });
    }
    Ok(())
}

fn suite_name<'a>(case: Node<'a, '_>) -> Option<&'a str> {
    case.ancestors().find(|n| n.has_tag_name("testsuite")).and_then(|n| n.attribute("name"))
}

fn outcome(case: Node<'_, '_>) -> TestOutcome {
    let has = |tag: &str| case.children().any(|c| c.has_tag_name(tag));
    if has("failure") || has("error") {
        TestOutcome::Fail
    } else if has("skipped") {
        TestOutcome::Skip
    } else {
        TestOutcome::Pass
    }
}

fn output(case: Node<'_, '_>) -> String {
    let mut parts: Vec<String> = Vec::new();
    for child in case.children().filter(|c| c.is_element()) {
        let tag = child.tag_name().name();
        if !matches!(tag, "failure" | "error" | "system-out" | "system-err") {
            continue;
        }
        if let Some(message) = child.attribute("message").map(str::trim).filter(|m| !m.is_empty()) {
            parts.push(message.to_string());
        }
        let text: String = child.descendants().filter_map(|n| n.text()).collect();
        if !text.trim().is_empty() {
            parts.push(text.trim().to_string());
        }
    }
    let joined = parts.join("\n");
    let skip = joined.chars().count().saturating_sub(OUTPUT_TAIL_CHARS);
    joined.chars().skip(skip).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUREFIRE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuite name="com.shop.CartTest" tests="4" failures="1" errors="1" skipped="1">
  <testcase name="total()" classname="com.shop.CartTest" time="0.01"/>
  <testcase name="tax()" classname="com.shop.CartTest" time="0.01">
    <failure message="expected: &lt;10&gt; but was: &lt;12&gt;" type="org.opentest4j.AssertionFailedError">at CartTest.tax(CartTest.java:21)</failure>
    <system-out><![CDATA[computing tax]]></system-out>
  </testcase>
  <testcase name="boom()" classname="com.shop.CartTest"><error message="NPE"/></testcase>
  <testcase name="later()" classname="com.shop.CartTest"><skipped/></testcase>
</testsuite>"#;

    #[test]
    fn surefire_report_maps_outcomes_and_failure_output() {
        let mut run = TestRun::default();
        parse_junit(SUREFIRE, &mut run).unwrap();
        assert_eq!(run.outcome("com.shop.CartTest", "total()"), Some(TestOutcome::Pass));
        assert_eq!(run.outcome("com.shop.CartTest", "tax()"), Some(TestOutcome::Fail));
        assert_eq!(run.outcome("com.shop.CartTest", "boom()"), Some(TestOutcome::Fail));
        assert_eq!(run.outcome("com.shop.CartTest", "later()"), Some(TestOutcome::Skip));
        let tax = run.result("com.shop.CartTest", "tax()").unwrap();
        assert!(tax.output_tail.contains("expected: <10> but was: <12>"));
        assert!(tax.output_tail.contains("CartTest.java:21"));
        assert!(tax.output_tail.contains("computing tax"));
    }

    #[test]
    fn pytest_report_with_testsuites_root_and_file_attribute() {
        let xml = r#"<testsuites><testsuite name="pytest">
            <testcase classname="tests.test_cart" name="test_total" file="tests/test_cart.py" line="3"/>
            <testcase name="test_orphan"/>
        </testsuite></testsuites>"#;
        let mut run = TestRun::default();
        parse_junit(xml, &mut run).unwrap();
        assert_eq!(run.outcome("tests.test_cart", "test_total"), Some(TestOutcome::Pass));
        assert_eq!(run.outcome("pytest", "test_orphan"), Some(TestOutcome::Pass));
    }

    #[test]
    fn malformed_or_hostile_xml_is_an_error_not_a_panic() {
        let mut run = TestRun::default();
        assert!(parse_junit("<testsuite><testcase", &mut run).is_err());
        let entity = r#"<!DOCTYPE x [<!ENTITY e SYSTEM "file:///etc/passwd">]><testsuite><testcase name="a">&e;</testcase></testsuite>"#;
        let _ = parse_junit(entity, &mut run);
        assert!(run.packages.values().flat_map(|t| t.values()).all(|r| !r.output_tail.contains("root:")));
    }

    #[test]
    fn long_output_keeps_the_tail() {
        let xml = format!(
            r#"<testsuite><testcase name="a"><failure message="m">{}END</failure></testcase></testsuite>"#,
            "x".repeat(OUTPUT_TAIL_CHARS * 2)
        );
        let mut run = TestRun::default();
        parse_junit(&xml, &mut run).unwrap();
        let out = &run.result("", "a").unwrap().output_tail;
        assert_eq!(out.chars().count(), OUTPUT_TAIL_CHARS);
        assert!(out.ends_with("END"));
    }
}
