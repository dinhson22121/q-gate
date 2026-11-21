//! What qgate extracts from one Figma frame.

use serde::{Deserialize, Serialize};

/// The comparable content of one Figma frame (node and its descendants).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DesignSpec {
    pub frame_name: String,
    /// API form, e.g. "12:345".
    pub node_id: String,
    /// `characters` of TEXT nodes, in document order, deduplicated.
    pub texts: Vec<String>,
    /// Design-system components used through instances.
    pub components: Vec<ComponentRef>,
    /// Variables (or styles) bound to properties of nodes in the frame.
    pub tokens: Vec<TokenRef>,
    /// Variant / state names found in the frame (e.g. "State=Loading", "Empty state").
    pub variants: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentRef {
    /// Component (or component set) name, e.g. "Button/Primary".
    pub name: String,
    /// Published component key, empty when unknown.
    pub key: String,
}

/// Which kind of property a token is bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenKind {
    Color,
    Spacing,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRef {
    /// Variable or style name, e.g. "color/brand/primary".
    pub name: String,
    /// Resolved value, e.g. "#3366ff" or "16".
    pub value: String,
    pub kind: TokenKind,
}

impl DesignSpec {
    pub fn tokens_of(&self, kind: TokenKind) -> impl Iterator<Item = &TokenRef> {
        self.tokens.iter().filter(move |t| t.kind == kind)
    }
}
