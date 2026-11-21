use chrono::DateTime;
use serde::Deserialize;
use serde_json::Value;

use crate::llm::{LlmError, Usage};

const STOP_REFUSAL: &str = "refusal";
const STOP_MAX_TOKENS: &str = "max_tokens";
const HTTP_TOO_MANY_REQUESTS: u16 = 429;
const HTTP_SERVER_ERROR_MIN: u16 = 500;
const RESET_MARKER: &str = "resets";

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct EnvelopeUsage {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct Envelope {
    is_error: bool,
    result: Option<String>,
    structured_output: Option<Value>,
    api_error_status: Option<u16>,
    stop_reason: Option<String>,
    total_cost_usd: f64,
    usage: EnvelopeUsage,
}

#[derive(Debug)]
pub(super) struct Answer {
    pub payload: Value,
    pub usage: Usage,
    pub cost_usd: f64,
}

impl Envelope {
    pub(super) fn parse(stdout: &str) -> Option<Self> {
        serde_json::from_str(stdout.trim()).ok()
    }

    pub(super) fn into_answer(self) -> Result<Answer, LlmError> {
        let text = self.result.clone().unwrap_or_default();
        if self.is_error {
            return Err(classify_failure(self.api_error_status, &text));
        }
        match self.stop_reason.as_deref() {
            Some(STOP_REFUSAL) => return Err(LlmError::Refused { category: None }),
            Some(STOP_MAX_TOKENS) => return Err(LlmError::Truncated),
            _ => {}
        }
        let (usage, cost_usd) = self.consumption();
        let payload = match self.structured_output {
            Some(value) => value,
            None => Value::String(text),
        };
        Ok(Answer { payload, usage, cost_usd })
    }

    fn consumption(&self) -> (Usage, f64) {
        let usage = Usage {
            input_tokens: self.usage.input_tokens,
            output_tokens: self.usage.output_tokens,
            cache_read_tokens: self.usage.cache_read_input_tokens,
            cache_write_tokens: self.usage.cache_creation_input_tokens,
        };
        (usage, self.total_cost_usd)
    }
}

pub(crate) fn envelope_consumption(stdout: &str) -> (Usage, f64) {
    Envelope::parse(stdout).map(|envelope| envelope.consumption()).unwrap_or_default()
}

pub(crate) fn classify_failure(status: Option<u16>, text: &str) -> LlmError {
    let text = text.trim();
    if status == Some(HTTP_TOO_MANY_REQUESTS) || looks_rate_limited(text) {
        return LlmError::RateLimited { resets_at: parse_reset_time(text), message: text.to_string() };
    }
    match status {
        Some(code) if code < HTTP_SERVER_ERROR_MIN => LlmError::Rejected(text.to_string()),
        _ => LlmError::Unavailable(text.to_string()),
    }
}

fn looks_rate_limited(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("usage limit") || (lower.contains("limit") && lower.contains(RESET_MARKER))
}

pub(super) fn parse_reset_time(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    if let Some(at) = lower.find(RESET_MARKER) {
        let rest = text[at + RESET_MARKER.len()..].trim_start_matches([' ', ':']);
        let line = rest.lines().next().unwrap_or_default();
        let time = line.trim().trim_end_matches('.').trim();
        if !time.is_empty() {
            return Some(time.to_string());
        }
    }
    let epoch: i64 = text.rsplit_once('|')?.1.trim().parse().ok()?;
    Some(DateTime::from_timestamp(epoch, 0)?.to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUCCESS: &str = include_str!("fixtures/structured_success.json");
    const MODEL_ERROR: &str = include_str!("fixtures/model_not_found_error.json");
    const RATE_LIMITED: &str = include_str!("fixtures/rate_limited_synthetic.json");

    fn answer(raw: &str) -> Result<Answer, LlmError> {
        Envelope::parse(raw).expect("fixture parses").into_answer()
    }

    #[test]
    fn real_success_envelope_yields_structured_output_and_usage() {
        let answer = answer(SUCCESS).unwrap();

        assert_eq!(answer.payload["name"], "qgate");
        assert_eq!(answer.usage.input_tokens, 2);
        assert_eq!(answer.usage.output_tokens, 134);
        assert_eq!(answer.usage.cache_write_tokens, 3605);
        assert_eq!(answer.usage.cache_read_tokens, 0);
        assert!(answer.cost_usd > 0.0);
    }

    #[test]
    fn plain_text_result_is_returned_as_string_payload() {
        let answer = answer(r#"{"is_error":false,"result":"hi"}"#).unwrap();
        assert_eq!(answer.payload, Value::String("hi".into()));
    }

    #[test]
    fn real_error_envelope_maps_4xx_to_rejected() {
        assert!(matches!(answer(MODEL_ERROR), Err(LlmError::Rejected(m)) if m.contains("nonexistent-model")));
    }

    #[test]
    fn rate_limit_fixture_is_detected_with_reset_time() {
        match answer(RATE_LIMITED) {
            Err(LlmError::RateLimited { resets_at, message }) => {
                assert_eq!(resets_at.as_deref(), Some("4:30pm (Asia/Ho_Chi_Minh)"));
                assert!(message.contains("hit your limit"));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn rate_limit_is_detected_from_wording_without_status() {
        let err = classify_failure(None, "5-hour limit reached ∙ resets 3pm.");
        assert_eq!(
            err,
            LlmError::RateLimited {
                resets_at: Some("3pm".into()),
                message: "5-hour limit reached ∙ resets 3pm.".into()
            }
        );
    }

    #[test]
    fn legacy_epoch_reset_is_rendered_as_rfc3339() {
        let err = classify_failure(Some(429), "Claude AI usage limit reached|1760000000");
        assert!(matches!(err, LlmError::RateLimited { resets_at: Some(t), .. } if t.starts_with("2025-10-09T")));
    }

    #[test]
    fn rate_limit_without_reset_time_keeps_message() {
        let err = classify_failure(Some(429), "slow down");
        assert_eq!(err, LlmError::RateLimited { resets_at: None, message: "slow down".into() });
        assert_eq!(parse_reset_time("nothing here"), None);
    }

    #[test]
    fn server_errors_and_unknown_failures_are_unavailable() {
        assert!(matches!(classify_failure(Some(529), "overloaded"), LlmError::Unavailable(_)));
        assert!(matches!(classify_failure(None, "boom"), LlmError::Unavailable(_)));
    }

    #[test]
    fn refusal_and_truncation_stop_reasons_are_typed() {
        assert_eq!(answer(r#"{"stop_reason":"refusal"}"#).unwrap_err(), LlmError::Refused { category: None });
        assert_eq!(answer(r#"{"stop_reason":"max_tokens"}"#).unwrap_err(), LlmError::Truncated);
    }

    #[test]
    fn non_json_stdout_does_not_parse() {
        assert!(Envelope::parse("Error: bad flag").is_none());
    }
}
