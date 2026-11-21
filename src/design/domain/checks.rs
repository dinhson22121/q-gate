use crate::review::application::{DesignMismatch, DesignMismatchKind};
use crate::review::domain::change_set::FileChange;

use super::spec::DesignSpec;
use super::text::normalize;
use super::ui::{changed_ui_files, is_style_file, reviewable};
use super::{component_check, copy_check, token_check};

pub type DesignCheckKind = DesignMismatchKind;

pub const ALL_CHECKS: [DesignCheckKind; 4] =
    [DesignCheckKind::Copy, DesignCheckKind::Component, DesignCheckKind::Token, DesignCheckKind::States];

pub fn detect_mismatches(
    spec: &DesignSpec,
    files: &[FileChange],
    i18n_texts: &[String],
    checks: &[DesignCheckKind],
) -> Vec<DesignMismatch> {
    let ui_files = changed_ui_files(files);
    let mut found = Vec::new();
    if checks.contains(&DesignCheckKind::Copy) && !ui_files.is_empty() {
        let i18n = normalize(&i18n_texts.join("\n"));
        found.extend(copy_check::check(spec, &ui_files, &i18n));
    }
    if checks.contains(&DesignCheckKind::Token) {
        let styled: Vec<&FileChange> =
            files.iter().filter(|f| reviewable(f) && is_style_file(&f.path)).chain(ui_files.iter().copied()).collect();
        found.extend(token_check::check(spec, &styled));
    }
    if checks.contains(&DesignCheckKind::Component) {
        found.extend(component_check::check(spec, &ui_files));
    }
    found
}

pub(super) fn mismatch(
    kind: DesignMismatchKind,
    spec: &DesignSpec,
    design_value: String,
    at: (&FileChange, u32, &str),
    explanation: String,
) -> DesignMismatch {
    let (file, line, evidence) = at;
    DesignMismatch {
        kind,
        frame: spec.frame_name.clone(),
        node_id: spec.node_id.clone(),
        design_value,
        file: file.path.clone(),
        line,
        code_evidence: evidence.trim().to_string(),
        explanation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::domain::fixtures::{spec, ui_file};
    use crate::design::domain::spec::{ComponentRef, TokenKind, TokenRef};

    #[test]
    fn runs_only_enabled_checks() {
        let mut design = spec(&["Pay securely now"]);
        design.tokens.push(TokenRef { name: "brand".into(), value: "#3366ff".into(), kind: TokenKind::Color });
        design.components.push(ComponentRef { name: "Button".into(), key: String::new() });
        let files = vec![ui_file("src/Pay.tsx", &["  <button style={{ color: '#3366ff' }}>Pay later</button>"])];
        let all = detect_mismatches(&design, &files, &[], &ALL_CHECKS);
        let kinds: Vec<_> = all.iter().map(|m| m.kind).collect();
        assert!(kinds.contains(&DesignCheckKind::Copy));
        assert!(kinds.contains(&DesignCheckKind::Token));
        assert!(kinds.contains(&DesignCheckKind::Component));
        let none = detect_mismatches(&design, &files, &[], &[DesignCheckKind::States]);
        assert!(none.is_empty());
    }

    #[test]
    fn non_ui_files_are_skipped() {
        let design = spec(&["Pay securely now"]);
        let files = vec![ui_file("server/pay.go", &["msg := \"Pay later\""])];
        assert!(detect_mismatches(&design, &files, &[], &[DesignCheckKind::Copy]).is_empty());
    }
}
