use serde_json::{Value, json};

use super::*;
use crate::conventions::domain::evidence::ChangedFile;
use crate::conventions::domain::rulebook::RuleStatus;
use crate::llm::FakeModel;

fn model(answer: Value) -> (ClaudeRuleModel, Arc<FakeModel>) {
    let fake = Arc::new(FakeModel::answering(answer));
    (ClaudeRuleModel::new(fake.clone()), fake)
}

fn rule() -> Rule {
    Rule {
        id: "R1".into(),
        title: "Domain is pure".into(),
        description: "No IO.".into(),
        kind: RuleKind::Explicit,
        status: RuleStatus::Approved,
        scope_globs: Vec::new(),
        examples: vec![FileLine { file: "CLAUDE.md".into(), line: 2, quote: "domain is pure".into() }],
        lint_suggestion: None,
    }
}

#[tokio::test]
async fn draft_call_sends_numbered_sources_and_parses_rules() {
    let answer = json!({"rules": [{
        "title": "Domain is pure", "description": "d", "kind": "explicit", "scope_globs": ["src/**"],
        "examples": [{"file": "CLAUDE.md", "line": 2, "quote": "domain is pure"}], "lint_suggestion": ""
    }]});
    let (model, fake) = model(answer);
    let request = DraftRequest {
        sources: vec![SourceFile { path: "CLAUDE.md".into(), content: "# R\ndomain is pure\n".into() }],
        samples: vec![SourceFile { path: "src/a\"x.rs".into(), content: "use a;\n".into() }],
        layout: vec!["src".into()],
        languages: vec![("rust".into(), 3)],
    };
    let drafts = model.draft_rules(&request).await.unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].kind, RuleKind::Explicit);
    assert_eq!(drafts[0].lint_suggestion, None);
    let calls = fake.calls.lock().unwrap();
    let (system, user, schema) = &calls[0];
    assert_eq!(system, BUILD_SYSTEM_PROMPT);
    assert_eq!(schema, &rules_schema());
    assert!(user.contains("<rule_source path=\"CLAUDE.md\">\n    1| # R\n    2| domain is pure\n</rule_source>"));
    assert!(user.contains("<code_sample path=\"src/a&quot;x.rs\">"));
    assert!(user.contains("rust: 3 files") && user.contains("src/\n"));
}

#[tokio::test]
async fn check_call_appends_the_rulebook_to_the_system_prompt_and_parses_violations() {
    let answer = json!({"violations": [{
        "rule_id": "R1", "file": "a.rs", "line": 2, "evidence": "std::fs::read()",
        "explanation": "IO in domain", "suggestion": "inject a port"
    }]});
    let (model, fake) = model(answer);
    let rules = [rule()];
    let files = [ChangedFile { path: "a.rs".into(), diff: "    2 + std::fs::read()\n".into(), new_lines: Vec::new() }];
    let request = CheckRequest { rules: &rules, selected: &["R1"], files: &files, context: Some("ticket") };
    let found = model.find_violations(request).await.unwrap();
    assert_eq!(found[0].rule_id, "R1");
    assert_eq!(found[0].suggestion.as_deref(), Some("inject a port"));
    let calls = fake.calls.lock().unwrap();
    let (system, user, schema) = &calls[0];
    assert!(system.starts_with(CHECK_SYSTEM_PROMPT));
    assert!(system.ends_with(&rulebook_text(&rules)));
    assert_eq!(schema, &violations_schema());
    assert!(user.starts_with("<rules_to_check>R1</rules_to_check>\n<ticket_context>\nticket\n</ticket_context>"));
    assert!(user.contains("<changed_file path=\"a.rs\">\n    2 + std::fs::read()\n</changed_file>"));
}

#[tokio::test]
async fn model_errors_are_mapped() {
    let fake = Arc::new(FakeModel::new(vec![Ok(json!({"unexpected": true})), Ok(json!({"unexpected": true}))]));
    let model = ClaudeRuleModel::new(fake);
    let rules = [rule()];
    let request = CheckRequest { rules: &rules, selected: &["R1"], files: &[], context: None };
    assert!(matches!(model.find_violations(request).await, Err(ConventionsError::Model(_))));
}

#[tokio::test]
async fn transport_errors_are_mapped() {
    let fake = Arc::new(FakeModel::new(vec![Err(LlmError::RateLimited { resets_at: None, message: "limit".into() })]));
    let request = DraftRequest { sources: vec![], samples: vec![], layout: vec![], languages: vec![] };
    let result = ClaudeRuleModel::new(fake).draft_rules(&request).await;
    assert!(matches!(result, Err(ConventionsError::Model(m)) if m.contains("usage limit")));
}

#[test]
fn rulebook_text_is_deterministic_and_escaped() {
    let mut scoped = rule();
    scoped.scope_globs = vec!["src/**".into(), "lib/\"x\"".into()];
    let text = rulebook_text(&[rule(), scoped]);
    assert_eq!(
        text,
        rulebook_text(&[rule(), {
            let mut again = rule();
            again.scope_globs = vec!["src/**".into(), "lib/\"x\"".into()];
            again
        }])
    );
    assert!(text.contains("scope=\"**\""));
    assert!(text.contains("scope=\"src/**, lib/&quot;x&quot;\""));
    assert!(text.contains("example CLAUDE.md:2: domain is pure"));
}
