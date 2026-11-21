use std::collections::HashSet;

use crate::review::application::{DesignMismatch, DesignMismatchKind};
use crate::review::domain::change_set::FileChange;

use super::checks::mismatch;
use super::spec::DesignSpec;
use super::ui::{added_lines, ignored_lines};

const MAX_COMPONENT_MISMATCHES: usize = 20;
const ROLES: [(&[&str], &[&str]); 8] = [
    (&["checkbox", "radio", "switch", "toggle"], &["input"]),
    (&["button"], &["button"]),
    (&["select", "dropdown", "combobox"], &["select"]),
    (&["textarea", "input", "textfield", "field"], &["input", "textarea"]),
    (&["link"], &["a"]),
    (&["image", "avatar", "img"], &["img"]),
    (&["modal", "dialog"], &["dialog"]),
    (&["table"], &["table"]),
];

pub(super) fn check(spec: &DesignSpec, files: &[&FileChange]) -> Vec<DesignMismatch> {
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    for component in &spec.components {
        let identifier = component_identifier(&component.name);
        let Some(elements) = raw_elements(&identifier) else { continue };
        if !seen.insert(identifier.clone()) || files.iter().any(|f| uses_identifier(f, &identifier)) {
            continue;
        }
        for file in files {
            if let Some((line, code, element)) = first_raw_element(file, elements) {
                let explanation = format!(
                    "The design uses the design-system component `{}`, but the change renders a raw `<{element}>` \
                     and does not use `{identifier}`.",
                    component.name
                );
                found.push(mismatch(
                    DesignMismatchKind::Component,
                    spec,
                    component.name.clone(),
                    (file, line, code),
                    explanation,
                ));
            }
        }
    }
    found.truncate(MAX_COMPONENT_MISMATCHES);
    found
}

fn component_identifier(name: &str) -> String {
    let base = name.split('/').next().unwrap_or_default();
    base.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            chars.next().map(|c| c.to_ascii_uppercase().to_string() + chars.as_str()).unwrap_or_default()
        })
        .collect()
}

fn raw_elements(identifier: &str) -> Option<&'static [&'static str]> {
    let lower = identifier.to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    ROLES.iter().find(|(fragments, _)| fragments.iter().any(|f| lower.contains(f))).map(|(_, elements)| *elements)
}

fn uses_identifier(file: &FileChange, identifier: &str) -> bool {
    file.new_lines().iter().any(|(_, text)| {
        text.match_indices(identifier).any(|(i, _)| {
            let before = text[..i].chars().next_back();
            let after = text[i + identifier.len()..].chars().next();
            let is_word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
            !is_word(before) && !is_word(after)
        })
    })
}

fn first_raw_element<'a>(file: &'a FileChange, elements: &[&'static str]) -> Option<(u32, &'a str, &'static str)> {
    let ignored = ignored_lines(file);
    added_lines(file).into_iter().filter(|(line, _)| !ignored.contains(line)).find_map(|(line, code)| {
        elements.iter().find(|element| has_tag(code, element)).map(|element| (line, code, *element))
    })
}

fn has_tag(code: &str, element: &str) -> bool {
    let tag = format!("<{element}");
    code.match_indices(&tag)
        .any(|(i, _)| code[i + tag.len()..].chars().next().is_none_or(|c| c.is_whitespace() || c == '>' || c == '/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::domain::fixtures::{spec, ui_file};
    use crate::design::domain::spec::ComponentRef;

    fn design(names: &[&str]) -> DesignSpec {
        let mut s = spec(&[]);
        s.components = names.iter().map(|n| ComponentRef { name: n.to_string(), key: "k".into() }).collect();
        s
    }

    #[test]
    fn normalizes_component_names() {
        assert_eq!(component_identifier("Button/Primary"), "Button");
        assert_eq!(component_identifier("\u{1F518} text field / large"), "TextField");
        assert_eq!(component_identifier("icon-button"), "IconButton");
        assert_eq!(raw_elements("Toggle"), Some(&["input"][..]));
        assert_eq!(raw_elements("Card"), None);
    }

    #[test]
    fn raw_element_instead_of_design_system_component_is_reported() {
        let file = ui_file("src/Pay.tsx", &["export const Pay = () => (", "  <button className=\"pay\">Pay</button>"]);
        let found = check(&design(&["Button/Primary", "Button/Secondary"]), &[&file]);
        assert_eq!(found.len(), 1, "one report per component identifier and file");
        assert_eq!(found[0].line, 2);
        assert_eq!(found[0].design_value, "Button/Primary");
        assert!(found[0].explanation.contains("<button>"));
    }

    #[test]
    fn using_the_component_or_unknown_roles_is_fine() {
        let used = ui_file("src/Pay.tsx", &["import { Button } from '@ds/core';", "<button hidden />"]);
        assert!(check(&design(&["Button/Primary"]), &[&used]).is_empty());
        let card = ui_file("src/Pay.tsx", &["<div className=\"card\"><button>x</button></div>"]);
        assert!(check(&design(&["Card"]), &[&card]).is_empty());
        let buttons_only = ui_file("src/Pay.tsx", &["<buttonish />", "const PrimaryButtonX = 1;"]);
        assert!(check(&design(&["Button"]), &[&buttons_only]).is_empty());
    }

    #[test]
    fn ignore_comment_suppresses_raw_element() {
        let file = ui_file("src/Pay.tsx", &["{/* qgate-design-ignore */}", "<input type=\"text\" />"]);
        assert!(check(&design(&["Text field"]), &[&file]).is_empty());
    }
}
