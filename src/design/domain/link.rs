use std::fmt;

const FIGMA_HOSTS: [&str; 2] = ["figma.com", "www.figma.com"];
const FILE_KINDS: [&str; 4] = ["design", "file", "proto", "board"];

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FigmaLink {
    pub file_key: String,
    pub node_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    #[error("not a Figma link")]
    NotFigma,
    #[error("Figma link has no file key")]
    MissingFileKey,
    #[error("Figma link points at a whole file; link a frame (node-id) instead")]
    MissingNode,
    #[error("Figma link has an invalid node id")]
    InvalidNode,
}

impl FigmaLink {
    pub fn parse(raw: &str) -> Result<Self, LinkError> {
        let rest = raw.trim().strip_prefix("https://").ok_or(LinkError::NotFigma)?;
        let (host_and_path, query) = rest.split_once('?').unwrap_or((rest, ""));
        let query = query.split('#').next().unwrap_or_default();
        let (host, path) = host_and_path.split_once('/').unwrap_or((host_and_path, ""));
        if !FIGMA_HOSTS.contains(&host.to_ascii_lowercase().as_str()) {
            return Err(LinkError::NotFigma);
        }
        let file_key = file_key(path)?;
        let node = query
            .split('&')
            .find_map(|pair| pair.strip_prefix("node-id="))
            .filter(|n| !n.is_empty())
            .ok_or(LinkError::MissingNode)?;
        Ok(Self { file_key, node_id: api_node_id(node)? })
    }
}

impl fmt::Display for FigmaLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.file_key, self.node_id)
    }
}

fn file_key(path: &str) -> Result<String, LinkError> {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if !segments.first().is_some_and(|kind| FILE_KINDS.contains(kind)) {
        return Err(LinkError::NotFigma);
    }
    let key = match segments.as_slice() {
        [_, _, "branch", branch, ..] => *branch,
        [_, key, ..] => *key,
        _ => return Err(LinkError::MissingFileKey),
    };
    if is_valid_file_key(key) { Ok(key.to_string()) } else { Err(LinkError::MissingFileKey) }
}

pub fn is_valid_file_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 64 && key.chars().all(|c| c.is_ascii_alphanumeric())
}

pub fn is_valid_node_id(id: &str) -> bool {
    let body = id.strip_prefix('I').unwrap_or(id);
    !body.is_empty()
        && body.len() <= 128
        && body.split(';').all(|part| part.split_once(':').is_some_and(|(a, b)| is_digits(a) && is_digits(b)))
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

fn api_node_id(raw: &str) -> Result<String, LinkError> {
    let decoded = raw.replace("%3A", ":").replace("%3a", ":").replace("%3B", ";").replace("%3b", ";");
    let id = if decoded.contains(':') { decoded } else { decoded.replace('-', ":") };
    if is_valid_node_id(&id) { Ok(id) } else { Err(LinkError::InvalidNode) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(file_key: &str, node_id: &str) -> FigmaLink {
        FigmaLink { file_key: file_key.into(), node_id: node_id.into() }
    }

    #[test]
    fn parses_design_links_and_converts_node_id() {
        let parsed = FigmaLink::parse("https://www.figma.com/design/AbC123/Checkout?node-id=12-345&t=x").unwrap();
        assert_eq!(parsed, link("AbC123", "12:345"));
        assert_eq!(parsed.to_string(), "AbC123#12:345");
        let file = FigmaLink::parse(" https://figma.com/file/Key9/Name?type=design&node-id=1%3A2 ").unwrap();
        assert_eq!(file, link("Key9", "1:2"));
    }

    #[test]
    fn branch_links_use_the_branch_key() {
        let parsed = FigmaLink::parse("https://www.figma.com/design/Main1/branch/Br4nch/Name?node-id=3-4").unwrap();
        assert_eq!(parsed, link("Br4nch", "3:4"));
    }

    #[test]
    fn rejects_foreign_hosts_http_and_missing_parts() {
        assert_eq!(FigmaLink::parse("https://evil.com/design/K/x?node-id=1-2"), Err(LinkError::NotFigma));
        assert_eq!(FigmaLink::parse("http://www.figma.com/design/K/x?node-id=1-2"), Err(LinkError::NotFigma));
        assert_eq!(FigmaLink::parse("https://www.figma.com/community/K"), Err(LinkError::NotFigma));
        assert_eq!(FigmaLink::parse("https://www.figma.com/design/K/x"), Err(LinkError::MissingNode));
        assert_eq!(FigmaLink::parse("https://www.figma.com/design/"), Err(LinkError::MissingFileKey));
        assert_eq!(FigmaLink::parse("https://www.figma.com/design/K../x?node-id=1-2"), Err(LinkError::MissingFileKey));
        assert_eq!(FigmaLink::parse("https://www.figma.com/design/K/x?node-id=a-b"), Err(LinkError::InvalidNode));
    }

    #[test]
    fn validates_node_ids() {
        assert!(is_valid_node_id("12:345"));
        assert!(is_valid_node_id("I1:2;3:4"));
        assert!(!is_valid_node_id("1:"));
        assert!(!is_valid_node_id("1:2&x=3"));
    }
}
