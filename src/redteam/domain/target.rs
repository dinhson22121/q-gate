use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Auth {
    Bearer { token: String },
    Login { url: String, username: String, password: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScanTarget {
    pub base_url: String,
    pub health_path: Option<String>,
    pub auth: Option<Auth>,
    pub allow_destructive: bool,
}

impl ScanTarget {
    pub fn host(&self) -> Option<String> {
        let url = url::Url::parse(&self.base_url).ok()?;
        url.host_str().map(str::to_string)
    }

    pub fn redacted(&self) -> RedactedTarget {
        RedactedTarget {
            base_url: self.base_url.clone(),
            health_path: self.health_path.clone(),
            authenticated: self.auth.is_some(),
            allow_destructive: self.allow_destructive,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactedTarget {
    pub base_url: String,
    pub health_path: Option<String>,
    pub authenticated: bool,
    pub allow_destructive: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_is_extracted_from_the_base_url() {
        let target = ScanTarget { base_url: "http://10.0.0.5:8080".into(), ..ScanTarget::default() };
        assert_eq!(target.host().as_deref(), Some("10.0.0.5"));
    }

    #[test]
    fn an_unparsable_url_has_no_host() {
        let target = ScanTarget { base_url: "not a url".into(), ..ScanTarget::default() };
        assert_eq!(target.host(), None);
    }

    #[test]
    fn redacted_drops_credentials_but_keeps_shape() {
        let target = ScanTarget {
            base_url: "http://127.0.0.1".into(),
            auth: Some(Auth::Bearer { token: "secret".into() }),
            ..ScanTarget::default()
        };
        let redacted = target.redacted();
        assert!(redacted.authenticated);
        assert_eq!(redacted.base_url, "http://127.0.0.1");
        let json = serde_json::to_string(&redacted).unwrap();
        assert!(!json.contains("secret"));
    }
}
