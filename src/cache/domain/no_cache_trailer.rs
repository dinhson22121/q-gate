const TRAILER_KEY: &str = "qgate";
const TRAILER_VALUE: &str = "no-cache";

pub struct NoCacheTrailer;

impl NoCacheTrailer {
    pub fn is_in(messages: &[String]) -> bool {
        messages.iter().flat_map(|m| m.lines()).any(Self::is_trailer_line)
    }

    fn is_trailer_line(line: &str) -> bool {
        line.split_once(':').is_some_and(|(key, value)| {
            key.trim().eq_ignore_ascii_case(TRAILER_KEY) && value.trim().eq_ignore_ascii_case(TRAILER_VALUE)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn trailer_is_found_case_insensitively_in_any_message() {
        assert!(NoCacheTrailer::is_in(&msgs(&["feat: x\n\nqgate: no-cache"])));
        assert!(NoCacheTrailer::is_in(&msgs(&["fix: y", "feat: x\n\nQGate:   No-Cache  \n"])));
    }

    #[test]
    fn other_text_is_not_a_trailer() {
        assert!(!NoCacheTrailer::is_in(&[]));
        assert!(!NoCacheTrailer::is_in(&msgs(&[
            "feat: no-cache layer",
            "qgate: cache",
            "qgate no-cache",
            "x-qgate: no-cache"
        ])));
        assert!(!NoCacheTrailer::is_in(&msgs(&["qgate: no-cache please"])));
    }
}
