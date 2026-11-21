use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DesignSpec {
    pub frame_name: String,
    pub node_id: String,
    pub texts: Vec<String>,
    pub components: Vec<ComponentRef>,
    pub tokens: Vec<TokenRef>,
    pub variants: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentRef {
    pub name: String,
    pub key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenKind {
    Color,
    Spacing,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRef {
    pub name: String,
    pub value: String,
    pub kind: TokenKind,
}

impl DesignSpec {
    pub fn tokens_of(&self, kind: TokenKind) -> impl Iterator<Item = &TokenRef> {
        self.tokens.iter().filter(move |t| t.kind == kind)
    }
}
