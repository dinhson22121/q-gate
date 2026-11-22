use std::time::Duration;

use super::evidence::{ExpectedEvidence, ProbeResponse};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub description: String,
}

pub fn verify(expected: &ExpectedEvidence, baseline_elapsed: Duration, response: &ProbeResponse) -> Option<Evidence> {
    match expected {
        ExpectedEvidence::BodyContains(needle) => verify_canary(needle, response),
        ExpectedEvidence::ExtraDelayAtLeastMs(threshold_ms) => {
            verify_timing_delay(*threshold_ms, baseline_elapsed, response)
        }
    }
}

fn verify_canary(needle: &str, response: &ProbeResponse) -> Option<Evidence> {
    if needle.trim().is_empty() {
        return None;
    }
    response.body.contains(needle).then(|| Evidence { description: format!("response body contains `{needle}`") })
}

fn verify_timing_delay(threshold_ms: u64, baseline_elapsed: Duration, response: &ProbeResponse) -> Option<Evidence> {
    let threshold = Duration::from_millis(threshold_ms);
    let extra = response.elapsed.checked_sub(baseline_elapsed).unwrap_or(Duration::ZERO);
    (extra >= threshold).then(|| Evidence {
        description: format!(
            "response delayed {}ms beyond the {}ms baseline (threshold {threshold_ms}ms)",
            extra.as_millis(),
            baseline_elapsed.as_millis()
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(body: &str, elapsed_ms: u64) -> ProbeResponse {
        ProbeResponse {
            status: 200,
            headers: Vec::new(),
            body: body.into(),
            elapsed: Duration::from_millis(elapsed_ms),
        }
    }

    #[test]
    fn canary_present_proves_the_hypothesis() {
        let evidence = verify(
            &ExpectedEvidence::BodyContains("QGATE-CANARY-7".into()),
            Duration::from_millis(5),
            &response("hello QGATE-CANARY-7 world", 5),
        );
        assert!(evidence.unwrap().description.contains("QGATE-CANARY-7"));
    }

    #[test]
    fn canary_absent_is_not_proven() {
        assert!(
            verify(&ExpectedEvidence::BodyContains("QGATE-CANARY-7".into()), Duration::ZERO, &response("hi", 5))
                .is_none()
        );
    }

    #[test]
    fn blank_canary_never_proves_anything() {
        assert!(verify(&ExpectedEvidence::BodyContains(String::new()), Duration::ZERO, &response("", 0)).is_none());
    }

    #[test]
    fn a_delay_at_or_above_threshold_proves_time_based_injection() {
        let evidence =
            verify(&ExpectedEvidence::ExtraDelayAtLeastMs(2000), Duration::from_millis(50), &response("", 2100));
        assert!(evidence.is_some());
    }

    #[test]
    fn a_delay_below_threshold_is_not_proven() {
        assert!(
            verify(&ExpectedEvidence::ExtraDelayAtLeastMs(2000), Duration::from_millis(50), &response("", 900))
                .is_none()
        );
    }
}
