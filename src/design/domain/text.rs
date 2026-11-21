//! Text normalisation, placeholder detection and similarity for the copy check.

use std::collections::HashSet;

/// Copy shorter than this (in characters) must have at least `MIN_WORDS` words to be checked.
const MIN_CHARS: usize = 8;
const MIN_WORDS: usize = 2;
/// Words that only make up placeholder copy ("Label", "Button text", "Lorem ipsum").
const PLACEHOLDER_WORDS: [&str; 22] = [
    "lorem",
    "ipsum",
    "text",
    "label",
    "title",
    "subtitle",
    "heading",
    "placeholder",
    "button",
    "description",
    "caption",
    "value",
    "name",
    "body",
    "link",
    "tag",
    "badge",
    "content",
    "header",
    "footer",
    "item",
    "here",
];
const HTML_ENTITIES: [(&str, &str); 6] =
    [("&apos;", "'"), ("&#39;", "'"), ("&quot;", "\""), ("&amp;", "&"), ("&nbsp;", " "), ("&#x27;", "'")];

/// Lowercase, typographic quotes and dashes unified, entities decoded, whitespace collapsed.
pub fn normalize(text: &str) -> String {
    let mut decoded = text.replace("\\'", "'").replace("\\\"", "\"");
    for (entity, plain) in HTML_ENTITIES {
        decoded = decoded.replace(entity, plain);
    }
    let unified: String = decoded
        .chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '`' => '\'',
            '\u{201C}' | '\u{201D}' => '"',
            '\u{2013}' | '\u{2014}' => '-',
            '\u{00A0}' | '\u{2007}' | '\u{202F}' => ' ',
            '\u{2026}' => '.',
            other => other,
        })
        .flat_map(char::to_lowercase)
        .collect();
    unified.replace("...", ".").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Words (alphanumeric runs) of a text, lowercased.
pub fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// True for copy worth comparing: real words, long enough, not placeholder, numbers or icons.
pub fn is_meaningful_copy(text: &str) -> bool {
    let normalized = normalize(text);
    if normalized.starts_with("lorem") || !normalized.chars().any(char::is_alphabetic) {
        return false; // filler text, numbers, prices, times, icons, punctuation
    }
    let all_words = words(&normalized);
    if all_words.len() < MIN_WORDS && normalized.chars().count() < MIN_CHARS {
        return false;
    }
    let is_placeholder_word =
        |w: &String| PLACEHOLDER_WORDS.contains(&w.as_str()) || w.chars().all(|c| c.is_ascii_digit());
    !all_words.iter().all(is_placeholder_word)
}

/// Jaccard overlap of the word sets of two texts (0.0..=1.0).
pub fn similarity(a: &str, b: &str) -> f64 {
    let a: HashSet<String> = words(a).into_iter().collect();
    let b: HashSet<String> = words(b).into_iter().collect();
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let shared = a.intersection(&b).count() as f64;
    shared / a.union(&b).count() as f64
}

/// String-ish pieces of one source line: quoted literals and JSX/HTML text between tags.
/// Falls back to the whole trimmed line (multi-line JSX text has no delimiters).
pub fn text_candidates(line: &str) -> Vec<String> {
    let mut found = quoted_literals(line);
    found.extend(tag_texts(line));
    found.retain(|s| s.chars().any(char::is_alphabetic));
    if found.is_empty() && !line.trim().is_empty() {
        found.push(line.trim().to_string());
    }
    found
}

fn quoted_literals(line: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let mut quote: Option<char> = None;
    let mut current = String::new();
    let mut escaped = false;
    for c in line.chars() {
        match quote {
            Some(_) if escaped => {
                current.push(c);
                escaped = false;
            }
            Some(_) if c == '\\' => escaped = true,
            Some(q) if c == q => {
                literals.push(std::mem::take(&mut current));
                quote = None;
            }
            Some(_) => current.push(c),
            None if matches!(c, '"' | '\'' | '`') => quote = Some(c),
            None => {}
        }
    }
    literals
}

fn tag_texts(line: &str) -> Vec<String> {
    line.split('>')
        .skip(1)
        .filter_map(|segment| segment.split('<').next())
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('{'))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_case_quotes_entities_and_whitespace() {
        assert_eq!(normalize("  Don\u{2019}t\n  PAY&nbsp;now\u{2026} "), "don't pay now.");
        assert_eq!(normalize("It&apos;s \\\"ok\\\""), "it's \"ok\"");
    }

    #[test]
    fn meaningful_copy_skips_placeholders_numbers_and_icons() {
        assert!(is_meaningful_copy("Pay now"));
        assert!(is_meaningful_copy("Checkout"));
        assert!(!is_meaningful_copy("Pay"));
        assert!(!is_meaningful_copy("Lorem ipsum dolor"));
        assert!(!is_meaningful_copy("Button text"));
        assert!(!is_meaningful_copy("Label 2"));
        assert!(!is_meaningful_copy("$1,299.00"));
        assert!(!is_meaningful_copy("→"));
        assert!(!is_meaningful_copy("12:30 - 13:45"));
    }

    #[test]
    fn similarity_is_word_overlap() {
        assert!((similarity("Pay now", "pay NOW") - 1.0).abs() < f64::EPSILON);
        assert!((similarity("Pay now", "Pay later") - 1.0 / 3.0).abs() < 1e-9);
        assert!(similarity("", "x").abs() < f64::EPSILON);
    }

    #[test]
    fn extracts_literals_and_tag_text() {
        assert_eq!(text_candidates(r#"<Button label="Pay now">Go 'on'</Button>"#), vec!["Pay now", "on", "Go 'on'"]);
        assert_eq!(text_candidates(r"const s = 'it\'s';"), vec!["it's"]);
        assert_eq!(text_candidates("    Continue to payment"), vec!["Continue to payment"]);
        assert!(text_candidates("   ").is_empty());
    }
}
