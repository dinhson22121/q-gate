const FIGMA_HOSTS: &[&str] = &["figma.com", "www.figma.com"];
const DESIGN_KINDS: &[&str] = &["design", "file", "proto"];
const URL_TERMINATORS: &[char] = &['"', '\'', '<', '>', '(', ')', '[', ']', '{', '}', '|', '\\', '`'];
const TRAILING_PUNCTUATION: &[char] = &['.', ',', ';', ':', '!', '?'];
const SCHEME: &str = "https://";

pub fn extract_figma_links(text: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut rest = text;
    while let Some(start) = find_ignore_case(rest, SCHEME) {
        let candidate = &rest[start..];
        let end =
            candidate.find(|c: char| c.is_whitespace() || URL_TERMINATORS.contains(&c)).unwrap_or(candidate.len());
        let url = candidate[..end].trim_end_matches(TRAILING_PUNCTUATION);
        if is_figma_design(url) && !links.iter().any(|l| l == url) {
            links.push(url.to_string());
        }
        rest = &candidate[SCHEME.len()..];
    }
    links
}

pub fn merge_links(links: &mut Vec<String>, more: impl IntoIterator<Item = String>) {
    for link in more {
        if !links.contains(&link) {
            links.push(link);
        }
    }
}

fn find_ignore_case(haystack: &str, needle: &str) -> Option<usize> {
    haystack.as_bytes().windows(needle.len()).position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

fn is_figma_design(url: &str) -> bool {
    let without_scheme = &url[SCHEME.len()..];
    let (host, path) = without_scheme.split_once('/').unwrap_or((without_scheme, ""));
    let mut segments = path.split(['/', '?', '#']);
    let kind = segments.next().unwrap_or_default();
    let file_key = segments.next().unwrap_or_default();
    FIGMA_HOSTS.contains(&host.to_ascii_lowercase().as_str())
        && DESIGN_KINDS.contains(&kind)
        && !file_key.is_empty()
        && file_key.bytes().all(|b| b.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_design_file_and_proto_links_in_text_and_markup() {
        let text = "Design: https://www.figma.com/design/AbC123/Checkout?node-id=12-345&t=x.\n\
                    [Proto|https://www.figma.com/proto/Zz9/Flow?node-id=1-2] and \
                    [old](https://figma.com/file/Q1w2/Legacy) \"https://WWW.FIGMA.COM/design/AbC123/Checkout?node-id=12-345&t=x\"";
        assert_eq!(
            extract_figma_links(text),
            vec![
                "https://www.figma.com/design/AbC123/Checkout?node-id=12-345&t=x",
                "https://www.figma.com/proto/Zz9/Flow?node-id=1-2",
                "https://figma.com/file/Q1w2/Legacy",
                "https://WWW.FIGMA.COM/design/AbC123/Checkout?node-id=12-345&t=x",
            ]
        );
    }

    #[test]
    fn ignores_boards_other_hosts_and_malformed_links() {
        let text = "https://www.figma.com/board/Abc/Brainstorm https://evil.com/design/Abc/x \
                    https://figma.com.evil.com/design/Abc https://www.figma.com/design/ http://www.figma.com/design/Abc/x \
                    https://www.figma.com/design/a-b/x https://www.figma.com https://";
        assert!(extract_figma_links(text).is_empty());
    }

    #[test]
    fn merges_without_duplicates() {
        let mut links = vec!["a".to_string()];
        merge_links(&mut links, vec!["b".to_string(), "a".to_string(), "b".to_string()]);
        assert_eq!(links, vec!["a", "b"]);
    }
}
