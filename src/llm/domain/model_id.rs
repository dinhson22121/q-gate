const MOVING_SUFFIX: &str = "-latest";
const CONTEXT_MARKER: char = '[';

pub fn is_pinned_model_id(model: &str) -> bool {
    let id = model.trim().to_ascii_lowercase();
    !id.is_empty()
        && id.chars().any(|c| c.is_ascii_digit())
        && !id.ends_with(MOVING_SUFFIX)
        && !id.contains(CONTEXT_MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_model_ids_are_pinned() {
        for id in ["claude-opus-5-5", " Claude-Haiku-5-5 ", "claude-sonnet-4-20250514"] {
            assert!(is_pinned_model_id(id), "{id}");
        }
    }

    #[test]
    fn aliases_moving_targets_and_context_variants_are_not_pinned() {
        for id in [
            "opus",
            "Sonnet",
            "haiku",
            "fable",
            "default",
            "best",
            "",
            "  ",
            "claude-opus-latest",
            "opus[1m]",
            "claude-opus-5-5[1m]",
            "claude-3-5-sonnet-latest",
        ] {
            assert!(!is_pinned_model_id(id), "{id}");
        }
    }
}
