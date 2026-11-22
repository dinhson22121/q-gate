use serde_json::Value;

use super::evidence::ProbeResponse;

const TOKEN_FIELDS: [&str; 3] = ["token", "access_token", "session_token"];

pub fn extract_session_headers(response: &ProbeResponse) -> Option<Vec<(String, String)>> {
    extract_cookie_header(response).or_else(|| extract_bearer_header(response))
}

fn extract_cookie_header(response: &ProbeResponse) -> Option<Vec<(String, String)>> {
    let crumbs: Vec<&str> = response
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
        .map(|(_, value)| value.split(';').next().unwrap_or("").trim())
        .filter(|crumb| !crumb.is_empty())
        .collect();
    if crumbs.is_empty() { None } else { Some(vec![("Cookie".to_string(), crumbs.join("; "))]) }
}

fn extract_bearer_header(response: &ProbeResponse) -> Option<Vec<(String, String)>> {
    let document: Value = serde_json::from_str(&response.body).ok()?;
    let token = TOKEN_FIELDS.iter().find_map(|field| document.get(field)).and_then(Value::as_str)?;
    Some(vec![("Authorization".to_string(), format!("Bearer {token}"))])
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn response(headers: Vec<(String, String)>, body: &str) -> ProbeResponse {
        ProbeResponse { status: 200, headers, body: body.into(), elapsed: Duration::ZERO }
    }

    #[test]
    fn a_single_session_cookie_becomes_a_cookie_header() {
        let out = extract_session_headers(&response(vec![("Set-Cookie".into(), "sid=abc; Path=/".into())], ""));
        assert_eq!(out, Some(vec![("Cookie".to_string(), "sid=abc".to_string())]));
    }

    #[test]
    fn multiple_set_cookie_headers_are_joined_into_one_cookie_header() {
        let out = extract_session_headers(&response(
            vec![("Set-Cookie".into(), "sid=abc; HttpOnly".into()), ("Set-Cookie".into(), "csrf=xyz".into())],
            "",
        ));
        assert_eq!(out, Some(vec![("Cookie".to_string(), "sid=abc; csrf=xyz".to_string())]));
    }

    #[test]
    fn a_json_token_body_becomes_a_bearer_header_when_there_is_no_cookie() {
        let out = extract_session_headers(&response(vec![], r#"{"token":"tok-1"}"#));
        assert_eq!(out, Some(vec![("Authorization".to_string(), "Bearer tok-1".to_string())]));
    }

    #[test]
    fn access_token_and_session_token_fields_are_also_recognized() {
        let out = extract_session_headers(&response(vec![], r#"{"access_token":"tok-2"}"#));
        assert_eq!(out, Some(vec![("Authorization".to_string(), "Bearer tok-2".to_string())]));
        let out = extract_session_headers(&response(vec![], r#"{"session_token":"tok-3"}"#));
        assert_eq!(out, Some(vec![("Authorization".to_string(), "Bearer tok-3".to_string())]));
    }

    #[test]
    fn a_cookie_takes_priority_over_a_body_token() {
        let out = extract_session_headers(&response(vec![("Set-Cookie".into(), "sid=abc".into())], r#"{"token":"t"}"#));
        assert_eq!(out, Some(vec![("Cookie".to_string(), "sid=abc".to_string())]));
    }

    #[test]
    fn neither_a_cookie_nor_a_token_yields_nothing() {
        assert_eq!(extract_session_headers(&response(vec![], "ok")), None);
    }
}
