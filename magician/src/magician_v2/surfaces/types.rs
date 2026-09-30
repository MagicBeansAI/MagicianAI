use serde::{Deserialize, Serialize};

pub const SURFACE_SCHEMA_VERSION: &str = "1.0";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SurfaceRequest {
    pub surface_version: String,
    pub target_route: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(default)]
    pub input_artifacts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication_mode: Option<String>,
}

impl Default for SurfaceRequest {
    fn default() -> Self {
        Self {
            surface_version: SURFACE_SCHEMA_VERSION.to_string(),
            target_route: String::new(),
            title: None,
            goal: None,
            input_artifacts: Vec::new(),
            publication_mode: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SurfaceSpec {
    pub surface_version: String,
    pub target_route: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub sections: Vec<serde_json::Value>,
    #[serde(default)]
    pub actions: Vec<serde_json::Value>,
}

impl Default for SurfaceSpec {
    fn default() -> Self {
        Self {
            surface_version: SURFACE_SCHEMA_VERSION.to_string(),
            target_route: String::new(),
            title: None,
            summary: None,
            sections: Vec::new(),
            actions: Vec::new(),
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn default_internal_surface_request_is_schema_versioned() {
        let request = SurfaceRequest::default();
        assert_eq!(request.surface_version, SURFACE_SCHEMA_VERSION);
        assert!(request.input_artifacts.is_empty());
    }
}
