use super::*;

fn events(kind: &str, verdicts: &[Verdict]) -> Vec<KindEvent> {
    verdicts.iter().map(|v| KindEvent { kind: kind.into(), verdict: *v }).collect()
}

fn dismissed(n: usize) -> Vec<Verdict> {
    vec![Verdict::Dismissed; n]
}

#[test]
fn replay_starts_at_100_and_counts() {
    assert_eq!(replay([]), (100, 0));
}

#[test]
fn replay_applies_steps_and_clamps_both_ends() {
    assert_eq!(replay([Verdict::Accepted]), (100, 1));
    assert_eq!(replay(dismissed(3)), (70, 3));
    assert_eq!(replay(dismissed(20)), (10, 20));
    assert_eq!(replay(dismissed(20).into_iter().chain([Verdict::Accepted])), (20, 21));
}

#[test]
fn replay_is_order_dependent_through_clamping() {
    let mut a = vec![Verdict::Accepted];
    a.extend(dismissed(2));
    let mut b = dismissed(2);
    b.push(Verdict::Accepted);
    assert_eq!(replay(a).0, 80);
    assert_eq!(replay(b).0, 90);
}

#[test]
fn inactive_below_min_judgments_has_no_cap() {
    let scores = score_kinds(&events("llm/performance", &dismissed(9)));
    assert_eq!(scores.len(), 1);
    assert_eq!(scores[0].score, 10);
    assert_eq!(scores[0].judgments, 9);
    assert!(!scores[0].active);
    assert_eq!(scores[0].cap, None);
}

#[test]
fn caps_by_thresholds_when_active() {
    let minor = score_kinds(&events("llm/performance", &dismissed(10)));
    assert_eq!((minor[0].score, minor[0].active, minor[0].cap), (10, true, Some(SeverityCap::Minor)));

    let mut at_20 = dismissed(10);
    at_20.extend([Verdict::Accepted]);
    let at_20 = score_kinds(&events("llm/performance", &at_20));
    assert_eq!((at_20[0].score, at_20[0].cap), (20, Some(SeverityCap::Minor)));

    let mut at_30 = dismissed(10);
    at_30.extend([Verdict::Accepted, Verdict::Accepted]);
    let at_30 = score_kinds(&events("llm/performance", &at_30));
    assert_eq!((at_30[0].score, at_30[0].cap), (30, Some(SeverityCap::Major)));
}

#[test]
fn boundary_scores_around_major_and_none() {
    assert_eq!(cap_for("llm/x", 50, true), Some(SeverityCap::Major));
    assert_eq!(cap_for("llm/x", 51, true), None);
    assert_eq!(cap_for("llm/x", 20, true), Some(SeverityCap::Minor));
    assert_eq!(cap_for("llm/x", 21, true), Some(SeverityCap::Major));
    assert_eq!(cap_for("llm/x", 10, false), None);
}

#[test]
fn security_is_scored_but_never_capped() {
    let scores = score_kinds(&events("llm/security", &dismissed(15)));
    assert_eq!((scores[0].score, scores[0].active, scores[0].cap), (10, true, None));
    assert_eq!(cap_for("tool:gitleaks/security", 10, true), None);
}

#[test]
fn groups_kinds_separately_sorted() {
    let mut all = events("llm/style", &dismissed(2));
    all.extend(events("llm/bug", &[Verdict::Accepted]));
    let scores = score_kinds(&all);
    assert_eq!(scores.iter().map(|s| s.kind.as_str()).collect::<Vec<_>>(), ["llm/bug", "llm/style"]);
    assert_eq!(scores[1].score, 80);
}

#[test]
fn category_extraction() {
    assert_eq!(category_of("llm/security"), "security");
    assert_eq!(category_of("nokind"), "nokind");
}
