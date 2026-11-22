use serde_json::Value;

use super::surface::{AttackSurface, Endpoint, EndpointSource};

const HTTP_METHODS: [&str; 7] = ["get", "post", "put", "delete", "patch", "head", "options"];

pub fn extend_from_openapi(surface: &mut AttackSurface, document: &Value) {
    let Some(paths) = document.get("paths").and_then(Value::as_object) else {
        return;
    };
    for (path, operations) in paths {
        let Some(operations) = operations.as_object() else {
            continue;
        };
        for method in HTTP_METHODS {
            if operations.contains_key(method) {
                surface.add(Endpoint::new(method.to_ascii_uppercase(), path.clone(), EndpointSource::OpenApi));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn extracts_every_declared_method_per_path() {
        let document = json!({
            "paths": {
                "/users": { "get": {}, "post": {} },
                "/users/{id}": { "delete": {} },
                "/ignored": "not an object"
            }
        });
        let mut surface = AttackSurface::default();
        extend_from_openapi(&mut surface, &document);
        let mut paths: Vec<(String, String)> =
            surface.endpoints.iter().map(|e| (e.method.clone(), e.path.clone())).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                ("DELETE".into(), "/users/{id}".into()),
                ("GET".into(), "/users".into()),
                ("POST".into(), "/users".into()),
            ]
        );
    }

    #[test]
    fn a_document_without_paths_adds_nothing() {
        let mut surface = AttackSurface::default();
        extend_from_openapi(&mut surface, &json!({}));
        assert!(surface.endpoints.is_empty());
    }
}
