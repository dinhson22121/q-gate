use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::design::application::{ComponentRef, DesignError, DesignSpec, TokenKind, TokenRef};

const MAX_NODES: usize = 20_000;
const MAX_ALIAS_DEPTH: usize = 5;
const STATE_WORDS: [&str; 10] =
    ["loading", "empty", "error", "disabled", "skeleton", "success", "hover", "focus", "pressed", "offline"];
const COLOR_PROPERTIES: [&str; 4] = ["fills", "strokes", "color", "fill"];
const SPACING_PROPERTIES: [&str; 6] =
    ["itemSpacing", "counterAxisSpacing", "paddingLeft", "paddingRight", "paddingTop", "paddingBottom"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundVariable {
    pub id: String,
    pub kind: TokenKind,
    pub fallback: Option<String>,
}

pub type Variables = HashMap<String, (String, Option<String>)>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedNodes {
    pub spec: DesignSpec,
    pub version: String,
    pub bound: Vec<BoundVariable>,
    pub truncated: bool,
}

struct Walker<'a> {
    components: &'a Map<String, Value>,
    component_sets: &'a Map<String, Value>,
    styles: &'a Map<String, Value>,
    spec: DesignSpec,
    bound: Vec<BoundVariable>,
    visited: usize,
}

pub fn parse_nodes(body: &Value, node_id: &str) -> Result<ParsedNodes, DesignError> {
    let entry = body
        .get("nodes")
        .and_then(|n| n.get(node_id))
        .filter(|e| !e.is_null())
        .ok_or_else(|| DesignError::NotFound(format!("node {node_id}")))?;
    let document = entry.get("document").ok_or_else(|| DesignError::Invalid("node without document".into()))?;
    let empty = Map::new();
    let map = |key: &str| entry.get(key).and_then(Value::as_object);
    let mut walker = Walker {
        components: map("components").unwrap_or(&empty),
        component_sets: map("componentSets").unwrap_or(&empty),
        styles: map("styles").unwrap_or(&empty),
        spec: DesignSpec {
            frame_name: str_of(document, "name").to_string(),
            node_id: node_id.to_string(),
            ..DesignSpec::default()
        },
        bound: Vec::new(),
        visited: 0,
    };
    walker.walk(document);
    let truncated = walker.visited > MAX_NODES;
    Ok(ParsedNodes { spec: walker.spec, version: version_of(body), bound: walker.bound, truncated })
}

pub fn version_of(body: &Value) -> String {
    match body.get("version") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

impl Walker<'_> {
    fn walk(&mut self, node: &Value) {
        self.visited += 1;
        if self.visited > MAX_NODES || node.get("visible").and_then(Value::as_bool) == Some(false) {
            return;
        }
        match str_of(node, "type") {
            "TEXT" => push_unique(&mut self.spec.texts, str_of(node, "characters").trim()),
            "INSTANCE" => self.instance(node),
            "COMPONENT_SET" => {
                for child in children(node).filter(|c| str_of(c, "type") == "COMPONENT") {
                    push_unique(&mut self.spec.variants, str_of(child, "name"));
                }
            }
            "FRAME" | "GROUP" | "SECTION" | "COMPONENT" if is_state_name(str_of(node, "name")) => {
                push_unique(&mut self.spec.variants, str_of(node, "name"));
            }
            _ => {}
        }
        self.bindings(node);
        self.styles_of(node);
        for child in children(node) {
            self.walk(child);
        }
    }

    fn instance(&mut self, node: &Value) {
        if let Some(component) = self.components.get(str_of(node, "componentId")) {
            let set =
                component.get("componentSetId").and_then(Value::as_str).and_then(|id| self.component_sets.get(id));
            let source = set.unwrap_or(component);
            let reference =
                ComponentRef { name: str_of(source, "name").to_string(), key: str_of(source, "key").to_string() };
            if !reference.name.is_empty() && !self.spec.components.iter().any(|c| c.name == reference.name) {
                self.spec.components.push(reference);
            }
        }
        if let Some(properties) = node.get("componentProperties").and_then(Value::as_object) {
            for (name, property) in properties.iter().filter(|(_, p)| str_of(p, "type") == "VARIANT") {
                let value = property.get("value").and_then(Value::as_str).unwrap_or_default();
                push_unique(&mut self.spec.variants, &format!("{}={value}", name.split('#').next().unwrap_or(name)));
            }
        }
    }

    fn bindings(&mut self, node: &Value) {
        if let Some(bound) = node.get("boundVariables").and_then(Value::as_object) {
            for (property, value) in bound {
                let kind = property_kind(property);
                let fallback = match kind {
                    TokenKind::Color => solid_color(node, property),
                    TokenKind::Spacing => node.get(property).and_then(Value::as_f64).map(format_number),
                    TokenKind::Other => None,
                };
                for id in alias_ids(value) {
                    self.bind(id, kind, fallback.clone());
                }
            }
        }
        for paint in node.get("fills").and_then(Value::as_array).into_iter().flatten() {
            if let Some(alias) = paint.get("boundVariables").and_then(|b| b.get("color")) {
                for id in alias_ids(alias) {
                    self.bind(id, TokenKind::Color, paint.get("color").and_then(hex_color));
                }
            }
        }
    }

    fn bind(&mut self, id: String, kind: TokenKind, fallback: Option<String>) {
        let binding = BoundVariable { id, kind, fallback };
        if !self.bound.contains(&binding) {
            self.bound.push(binding);
        }
    }

    fn styles_of(&mut self, node: &Value) {
        let Some(styles) = node.get("styles").and_then(Value::as_object) else { return };
        for (property, style_id) in styles {
            let kind = property_kind(property);
            let Some(style) = style_id.as_str().and_then(|id| self.styles.get(id)) else { continue };
            let (Some(value), TokenKind::Color) = (solid_color(node, property), kind) else { continue };
            let token = TokenRef { name: str_of(style, "name").to_string(), value, kind };
            if !token.name.is_empty() && !self.spec.tokens.contains(&token) {
                self.spec.tokens.push(token);
            }
        }
    }
}

pub fn resolve_tokens(parsed: &mut ParsedNodes, variables: Option<&Variables>) {
    for binding in &parsed.bound {
        let known = variables.and_then(|v| v.get(&binding.id));
        let name = known.map_or_else(|| format!("bound variable {}", binding.id), |(name, _)| name.clone());
        let Some(value) = known.and_then(|(_, value)| value.clone()).or_else(|| binding.fallback.clone()) else {
            continue;
        };
        let token = TokenRef { name, value, kind: binding.kind };
        if !parsed.spec.tokens.contains(&token) {
            parsed.spec.tokens.push(token);
        }
    }
}

pub fn parse_variables(body: &Value) -> Variables {
    let empty = Map::new();
    let meta = body.get("meta");
    let section = |key: &str| meta.and_then(|m| m.get(key)).and_then(Value::as_object).unwrap_or(&empty);
    let (variables, collections) = (section("variables"), section("variableCollections"));
    variables
        .iter()
        .map(|(id, variable)| {
            let value = resolve_value(variable, variables, collections, 0);
            (id.clone(), (str_of(variable, "name").to_string(), value))
        })
        .collect()
}

fn resolve_value(
    variable: &Value,
    variables: &Map<String, Value>,
    collections: &Map<String, Value>,
    depth: usize,
) -> Option<String> {
    let modes = variable.get("valuesByMode")?.as_object()?;
    let default_mode = collections
        .get(str_of(variable, "variableCollectionId"))
        .and_then(|c| c.get("defaultModeId"))
        .and_then(Value::as_str);
    let value = default_mode.and_then(|m| modes.get(m)).or_else(|| modes.values().next())?;
    if str_of(value, "type") == "VARIABLE_ALIAS" {
        let target = variables.get(str_of(value, "id"))?;
        return (depth < MAX_ALIAS_DEPTH).then(|| resolve_value(target, variables, collections, depth + 1)).flatten();
    }
    match value {
        Value::Number(n) => n.as_f64().map(format_number),
        Value::String(s) => Some(s.clone()),
        Value::Object(_) => hex_color(value),
        _ => None,
    }
}

fn property_kind(property: &str) -> TokenKind {
    if COLOR_PROPERTIES.contains(&property) || property.ends_with("Color") {
        TokenKind::Color
    } else if SPACING_PROPERTIES.contains(&property) {
        TokenKind::Spacing
    } else {
        TokenKind::Other
    }
}

fn alias_ids(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items.iter().flat_map(alias_ids).collect(),
        Value::Object(_) if str_of(value, "type") == "VARIABLE_ALIAS" => vec![str_of(value, "id").to_string()],
        Value::Object(map) => map.values().flat_map(alias_ids).collect(),
        _ => Vec::new(),
    }
}

fn solid_color(node: &Value, property: &str) -> Option<String> {
    let paints = if property.starts_with("stroke") { "strokes" } else { "fills" };
    node.get(paints)?
        .as_array()?
        .iter()
        .filter(|p| str_of(p, "type") == "SOLID" && p.get("visible").and_then(Value::as_bool) != Some(false))
        .find_map(|p| p.get("color").and_then(hex_color))
}

fn hex_color(color: &Value) -> Option<String> {
    let channel = |key: &str| color.get(key).and_then(Value::as_f64).map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
    Some(format!("#{:02x}{:02x}{:02x}", channel("r")?, channel("g")?, channel("b")?))
}

fn format_number(value: f64) -> String {
    let text = format!("{value:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn is_state_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    STATE_WORDS.iter().any(|w| lower.contains(w))
}

fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn children(node: &Value) -> impl Iterator<Item = &Value> {
    node.get("children").and_then(Value::as_array).into_iter().flatten()
}

fn push_unique(list: &mut Vec<String>, value: &str) {
    if !value.is_empty() && !list.iter().any(|v| v == value) {
        list.push(value.to_string());
    }
}

#[cfg(test)]
#[path = "figma_parse_tests.rs"]
pub(crate) mod tests;
