use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointSource {
    HealthCheck,
    CommonPath,
    OpenApi,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub method: String,
    pub path: String,
    pub source: EndpointSource,
}

impl Endpoint {
    pub fn new(method: impl Into<String>, path: impl Into<String>, source: EndpointSource) -> Self {
        Self { method: method.into(), path: path.into(), source }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttackSurface {
    pub endpoints: Vec<Endpoint>,
}

impl AttackSurface {
    pub fn add(&mut self, endpoint: Endpoint) {
        if !self.endpoints.iter().any(|e| e.method == endpoint.method && e.path == endpoint.path) {
            self.endpoints.push(endpoint);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adding_the_same_endpoint_twice_keeps_it_once() {
        let mut surface = AttackSurface::default();
        surface.add(Endpoint::new("GET", "/api", EndpointSource::CommonPath));
        surface.add(Endpoint::new("GET", "/api", EndpointSource::OpenApi));
        surface.add(Endpoint::new("POST", "/api", EndpointSource::OpenApi));
        assert_eq!(surface.endpoints.len(), 2);
    }
}
