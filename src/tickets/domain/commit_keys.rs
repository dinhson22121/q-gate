//! Which tickets a range of commits implements, as declared by its commit messages.
//!
//! A message *implements* the keys at the start of its subject line (`SHOP-7 SHOP-9: …`,
//! `[SHOP-7][SHOP-9] …`, `feat(SHOP-7): …`) and the keys of its `Jira:` / `Implements:` trailers.
//! A message that declares none implements the branch's key. Any other uppercase key in the message
//! (body mentions, `Refs:` trailers) is only a *reference*.

use super::ticket_key::{TicketKey, branch_key, exact_key, find_keys};

/// At most this many tickets are checked for one review.
pub const MAX_IMPLEMENTED_KEYS: usize = 5;
/// Trailer names (lowercase) whose keys are implemented by the commit.
const IMPLEMENTS_TRAILERS: &[&str] = &["jira", "implements"];
/// Characters allowed between leading keys of a subject line.
const LEADING_SEPARATORS: &[char] = &[' ', '\t', ',', ';', ':', '|', '&', '+', '/'];

/// Keys found in the commit messages of a reviewed range.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImplementedKeys {
    /// Tickets whose acceptance criteria the change must satisfy, deduplicated, at most
    /// [`MAX_IMPLEMENTED_KEYS`], in order of first appearance.
    pub implements: Vec<TicketKey>,
    /// Tickets only mentioned (informational), excluding implemented ones.
    pub references: Vec<TicketKey>,
    /// Whether more than [`MAX_IMPLEMENTED_KEYS`] implemented keys were found.
    pub capped: bool,
}

/// Collects implemented and referenced keys over all `messages` of a range (in the given order).
/// `allowed_projects` restricts the projects as in
/// [`extract_ticket_key`](super::ticket_key::extract_ticket_key).
pub fn extract_implemented_keys(
    branch: Option<&str>,
    messages: &[String],
    allowed_projects: &[String],
) -> ImplementedKeys {
    let from_branch = branch_key(branch, allowed_projects);
    let mut implements = Vec::new();
    let mut mentioned = Vec::new();
    let mut needs_branch_key = messages.is_empty();
    for message in messages {
        let declared = declared_keys(message, allowed_projects);
        needs_branch_key |= declared.is_empty();
        implements.extend(declared);
        mentioned.extend(find_keys(message, false, allowed_projects));
    }
    if needs_branch_key {
        implements.extend(from_branch);
    }
    let mut implements = dedup(implements);
    let capped = implements.len() > MAX_IMPLEMENTED_KEYS;
    implements.truncate(MAX_IMPLEMENTED_KEYS);
    let references = dedup(mentioned).into_iter().filter(|key| !implements.contains(key)).collect();
    ImplementedKeys { implements, references, capped }
}

/// Keys the message declares: leading subject keys, then `Jira:` / `Implements:` trailer keys.
fn declared_keys(message: &str, allowed_projects: &[String]) -> Vec<TicketKey> {
    let mut lines = message.lines().skip_while(|line| line.trim().is_empty());
    let subject = lines.next().unwrap_or_default();
    let mut keys = leading_keys(subject, allowed_projects);
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue };
        if IMPLEMENTS_TRAILERS.contains(&name.trim().to_ascii_lowercase().as_str()) {
            keys.extend(find_keys(value, true, allowed_projects));
        }
    }
    keys
}

/// Keys at the very start of a subject line, after an optional conventional-commit type whose
/// scope may itself hold keys (`feat(SHOP-7): …`). Bracketed tags that are not keys (`[WIP]`) are skipped.
fn leading_keys(subject: &str, allowed_projects: &[String]) -> Vec<TicketKey> {
    let (mut keys, mut rest) = match conventional_prefix(subject) {
        Some((scope, rest)) => (find_keys(scope, false, allowed_projects), rest),
        None => (Vec::new(), subject),
    };
    loop {
        rest = rest.trim_start_matches(LEADING_SEPARATORS);
        if let Some((inner, after)) = bracketed(rest) {
            keys.extend(inner.split(LEADING_SEPARATORS).filter_map(|token| exact_key(token, allowed_projects)));
            rest = after;
            continue;
        }
        let token_end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-')).unwrap_or(rest.len());
        match exact_key(&rest[..token_end], allowed_projects) {
            Some(key) => {
                keys.push(key);
                rest = &rest[token_end..];
            }
            None => return keys,
        }
    }
}

/// `type(scope)!: rest` → (`scope`, `rest`); `type: rest` → ("", `rest`).
fn conventional_prefix(subject: &str) -> Option<(&str, &str)> {
    let type_end = subject.find(|c: char| !c.is_ascii_alphabetic()).filter(|&end| end > 0)?;
    let after_type = &subject[type_end..];
    let (scope, after_scope) = match after_type.strip_prefix('(') {
        Some(inner) => inner.split_once(')')?,
        None => ("", after_type),
    };
    let rest = after_scope.strip_prefix('!').unwrap_or(after_scope).strip_prefix(':')?;
    Some((scope, rest))
}

/// `[inner] rest` or `(inner) rest`.
fn bracketed(text: &str) -> Option<(&str, &str)> {
    let close = match text.chars().next()? {
        '[' => ']',
        '(' => ')',
        _ => return None,
    };
    text[1..].split_once(close)
}

fn dedup(keys: Vec<TicketKey>) -> Vec<TicketKey> {
    let mut unique: Vec<TicketKey> = Vec::with_capacity(keys.len());
    for key in keys {
        if !unique.contains(&key) {
            unique.push(key);
        }
    }
    unique
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(branch: Option<&str>, messages: &[&str], allowed: &[&str]) -> (Vec<String>, Vec<String>, bool) {
        let messages: Vec<String> = messages.iter().map(|m| m.to_string()).collect();
        let allowed: Vec<String> = allowed.iter().map(|p| p.to_string()).collect();
        let found = extract_implemented_keys(branch, &messages, &allowed);
        let names = |keys: Vec<TicketKey>| keys.iter().map(ToString::to_string).collect();
        (names(found.implements), names(found.references), found.capped)
    }

    fn implements(branch: Option<&str>, messages: &[&str]) -> Vec<String> {
        keys(branch, messages, &[]).0
    }

    #[test]
    fn leading_subject_keys_are_implemented() {
        assert_eq!(implements(None, &["SHOP-7 SHOP-9: add cart"]), vec!["SHOP-7", "SHOP-9"]);
        assert_eq!(implements(None, &["[SHOP-7][SHOP-9] add cart"]), vec!["SHOP-7", "SHOP-9"]);
        assert_eq!(implements(None, &["SHOP-7, SHOP-9 | add cart"]), vec!["SHOP-7", "SHOP-9"]);
        assert_eq!(implements(None, &["[WIP] [SHOP-7, SHOP-8] x"]), vec!["SHOP-7", "SHOP-8"]);
        assert_eq!(implements(None, &["\n\nSHOP-7: x"]), vec!["SHOP-7"]);
        assert_eq!(implements(None, &["feat(SHOP-7): x"]), vec!["SHOP-7"]);
        assert_eq!(implements(None, &["fix!: SHOP-7 crash"]), vec!["SHOP-7"]);
    }

    #[test]
    fn subject_mentions_after_text_and_body_mentions_are_references() {
        let (implemented, referenced, _) =
            keys(None, &["Fix crash (refs OPS-1001)\n\nSee also SHOP-3 and shop-4.\nRefs: SHOP-5"], &[]);
        assert!(implemented.is_empty());
        assert_eq!(referenced, vec!["OPS-1001", "SHOP-3", "SHOP-5"]);
    }

    #[test]
    fn trailers_declare_implemented_keys() {
        let message = "Add cart\n\nBody mentions SHOP-1.\n\nJira: SHOP-7, shop-8\nImplements: OPS-2\nRefs: SHOP-9";
        let (implemented, referenced, _) = keys(Some("feature/SHOP-99-x"), &[message], &[]);
        assert_eq!(implemented, vec!["SHOP-7", "SHOP-8", "OPS-2"]);
        assert_eq!(referenced, vec!["SHOP-1", "SHOP-9"]);
    }

    #[test]
    fn branch_key_applies_when_a_message_declares_none() {
        assert_eq!(implements(Some("feature/shop-7-cart"), &["tidy up"]), vec!["SHOP-7"]);
        assert_eq!(implements(Some("feature/shop-7-cart"), &[]), vec!["SHOP-7"]);
        assert_eq!(implements(Some("feature/shop-7-cart"), &["SHOP-9: other"]), vec!["SHOP-9"]);
        assert!(implements(Some("main"), &["tidy"]).is_empty());
    }

    #[test]
    fn collects_over_the_whole_range_with_dedup() {
        let range = ["SHOP-9: second", "fix typo mentioning SHOP-9", "SHOP-7: first", "SHOP-9 again"];
        let (implemented, referenced, capped) = keys(Some("feature/SHOP-1"), &range, &[]);
        assert_eq!(implemented, vec!["SHOP-9", "SHOP-7", "SHOP-1"]);
        assert!(referenced.is_empty());
        assert!(!capped);
    }

    #[test]
    fn caps_implemented_keys() {
        let (implemented, _, capped) = keys(None, &["A1-1 A1-2 A1-3: x", "A1-4 A1-5 A1-6: y"], &[]);
        assert_eq!(implemented.len(), MAX_IMPLEMENTED_KEYS);
        assert!(capped);
        assert_eq!(implemented.last().map(String::as_str), Some("A1-5"));
    }

    #[test]
    fn respects_allow_list_and_deny_list() {
        let (implemented, referenced, _) =
            keys(Some("release-2"), &["SHOP-7 ABC-1: x\n\nsee ABC-2, SHOP-8"], &["shop"]);
        assert_eq!(implemented, vec!["SHOP-7"]);
        assert_eq!(referenced, vec!["SHOP-8"]);
        assert!(implements(None, &["UTF-8 support"]).is_empty());
    }

    #[test]
    fn lowercase_or_glued_subject_tokens_are_not_keys() {
        assert!(implements(None, &["shop-7: x"]).is_empty());
        assert!(implements(None, &["SHOP-7x: x"]).is_empty());
        assert!(implements(None, &["(unclosed SHOP-7"]).is_empty());
        assert!(implements(None, &["feat(SHOP-7 no close"]).is_empty());
        assert!(implements(None, &[""]).is_empty());
    }
}
