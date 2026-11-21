use serde_json::json;

use super::*;

pub(crate) fn nodes_body() -> Value {
    json!({
        "name": "Shop",
        "version": "4242",
        "nodes": { "12:345": {
            "document": {
                "id": "12:345", "name": "Checkout / Payment", "type": "FRAME",
                "fills": [{ "type": "SOLID", "color": { "r": 1, "g": 1, "b": 1, "a": 1 } }],
                "styles": { "fill": "S:bg" },
                "boundVariables": { "itemSpacing": { "type": "VARIABLE_ALIAS", "id": "VariableID:1:2" } },
                "itemSpacing": 16,
                "children": [
                    { "id": "1:1", "type": "TEXT", "name": "title", "characters": " Pay securely now " },
                    { "id": "1:9", "type": "TEXT", "name": "hidden", "characters": "Secret", "visible": false },
                    { "id": "1:2", "type": "INSTANCE", "name": "cta", "componentId": "C:1",
                      "componentProperties": { "State#1:0": { "type": "VARIANT", "value": "Loading" },
                                               "Label#2:0": { "type": "TEXT", "value": "Pay" } },
                      "fills": [{ "type": "SOLID", "color": { "r": 0.2, "g": 0.4, "b": 1, "a": 1 },
                                  "boundVariables": { "color": { "type": "VARIABLE_ALIAS", "id": "VariableID:1:3" } } }],
                      "children": [ { "id": "1:3", "type": "TEXT", "characters": "Pay securely now" } ] },
                    { "id": "1:4", "type": "FRAME", "name": "Empty state", "children": [] }
                ]
            },
            "components": { "C:1": { "key": "ck", "name": "State=Loading", "componentSetId": "CS:1" } },
            "componentSets": { "CS:1": { "key": "csk", "name": "Button/Primary" } },
            "styles": { "S:bg": { "name": "Surface/Default", "styleType": "FILL" } }
        } }
    })
}

pub(crate) fn variables_body() -> Value {
    json!({ "status": 200, "meta": {
        "variableCollections": { "VC:1": { "defaultModeId": "m1" } },
        "variables": {
            "VariableID:1:2": { "name": "space/md", "variableCollectionId": "VC:1",
                                 "valuesByMode": { "m1": 16, "m2": 24 } },
            "VariableID:1:3": { "name": "color/brand/primary", "variableCollectionId": "VC:1",
                                 "valuesByMode": { "m1": { "type": "VARIABLE_ALIAS", "id": "VariableID:9:9" } } },
            "VariableID:9:9": { "name": "blue/500", "variableCollectionId": "VC:1",
                                 "valuesByMode": { "m1": { "r": 0.2, "g": 0.4, "b": 1, "a": 1 } } }
        }
    } })
}

#[test]
fn parses_texts_components_variants_and_bindings() {
    let parsed = parse_nodes(&nodes_body(), "12:345").unwrap();
    assert_eq!(parsed.version, "4242");
    assert!(!parsed.truncated);
    let spec = &parsed.spec;
    assert_eq!(spec.frame_name, "Checkout / Payment");
    assert_eq!(spec.texts, vec!["Pay securely now"]);
    assert_eq!(spec.components, vec![ComponentRef { name: "Button/Primary".into(), key: "csk".into() }]);
    assert_eq!(spec.variants, vec!["State=Loading", "Empty state"]);
    assert_eq!(
        spec.tokens,
        vec![TokenRef { name: "Surface/Default".into(), value: "#ffffff".into(), kind: TokenKind::Color }]
    );
    assert_eq!(parsed.bound.len(), 2);
}

#[test]
fn resolves_variable_names_and_aliases() {
    let mut parsed = parse_nodes(&nodes_body(), "12:345").unwrap();
    resolve_tokens(&mut parsed, Some(&parse_variables(&variables_body())));
    let tokens: Vec<(&str, &str)> = parsed.spec.tokens.iter().map(|t| (t.name.as_str(), t.value.as_str())).collect();
    assert!(tokens.contains(&("space/md", "16")));
    assert!(tokens.contains(&("color/brand/primary", "#3366ff")));
}

#[test]
fn falls_back_to_node_values_without_variables() {
    let mut parsed = parse_nodes(&nodes_body(), "12:345").unwrap();
    resolve_tokens(&mut parsed, None);
    let spacing: Vec<&TokenRef> = parsed.spec.tokens_of(TokenKind::Spacing).collect();
    assert_eq!(spacing[0].name, "bound variable VariableID:1:2");
    assert_eq!(spacing[0].value, "16");
    assert!(parsed.spec.tokens.iter().any(|t| t.value == "#3366ff"));
}

#[test]
fn missing_node_and_versions() {
    assert!(matches!(parse_nodes(&json!({"nodes": {"1:2": null}}), "1:2"), Err(DesignError::NotFound(_))));
    assert!(matches!(parse_nodes(&json!({"nodes": {"1:2": {}}}), "1:2"), Err(DesignError::Invalid(_))));
    assert_eq!(version_of(&json!({"version": 7})), "7");
    assert_eq!(version_of(&json!({})), "");
    assert_eq!(format_number(12.5), "12.5");
}

#[test]
fn component_set_children_are_variants() {
    let body = json!({"nodes": {"1:1": {"document": {"type": "COMPONENT_SET", "name": "Card", "children": [
        {"type": "COMPONENT", "name": "State=Default"}, {"type": "COMPONENT", "name": "State=Error"}]}}}});
    assert_eq!(parse_nodes(&body, "1:1").unwrap().spec.variants, vec!["State=Default", "State=Error"]);
}
