//! Parameter compatibility utilities for strategy exploration
//!
//! This module provides utilities for calculating parameter compatibility
//! between available parameters and tool requirements.

use std::collections::HashMap;

use serde_json::Value;

/// Calculate parameter compatibility score between available parameters
/// and a tool's required parameters.
///
/// Returns 0.0-1.0 score representing what percentage of required parameters
/// are available.
///
/// # Arguments
/// * `tool_schema` - Tool's JSON schema with required parameters
/// * `available_params` - Parameters available from query extraction and tree
///   inheritance
///
/// # Returns
/// * `f32` - Coverage score from 0.0 (no params) to 1.0 (all params available)
///
/// # Example
/// ```
/// use std::collections::HashMap;
/// use serde_json::json;
/// use magician::magician_v2::strategy::parameter_utils::calculate_parameter_compatibility;
///
/// let schema = json!({
///     "required": ["target", "port"]
/// });
///
/// let mut available = HashMap::new();
/// available.insert("target".to_string(), "example.com".to_string());
/// available.insert("port".to_string(), "443".to_string());
///
/// let compatibility = calculate_parameter_compatibility(&schema, &available);
/// assert_eq!(compatibility, 1.0); // All required params available
/// ```
pub fn calculate_parameter_compatibility(
    tool_schema: &Value,
    available_params: &HashMap<String, String>,
) -> f32 {
    let required_params = extract_required_params(tool_schema);

    if required_params.is_empty() {
        return 1.0; // No required params = fully compatible
    }

    // Count how many required params are available
    let satisfied = required_params
        .iter()
        .filter(|&param| available_params.contains_key(param))
        .count();

    satisfied as f32 / required_params.len() as f32
}

/// Extract required parameter names from tool's JSON schema.
///
/// Checks both the "required" array and individual property "required" fields.
///
/// # Arguments
/// * `schema` - Tool's JSON schema
///
/// # Returns
/// * `Vec<String>` - List of required parameter names
///
/// # Example
/// ```
/// use serde_json::json;
/// use magician::magician_v2::strategy::parameter_utils::extract_required_params;
///
/// let schema = json!({
///     "type": "object",
///     "properties": {
///         "target": {"type": "string"},
///         "port": {"type": "number"}
///     },
///     "required": ["target", "port"]
/// });
///
/// let required = extract_required_params(&schema);
/// assert_eq!(required.len(), 2);
/// assert!(required.contains(&"target".to_string()));
/// assert!(required.contains(&"port".to_string()));
/// ```
pub fn extract_required_params(schema: &Value) -> Vec<String> {
    let mut required = Vec::new();

    // Check if schema has "required" array
    if let Some(required_array) = schema.get("required").and_then(|v| v.as_array()) {
        for item in required_array {
            if let Some(param_name) = item.as_str() {
                required.push(param_name.to_string());
            }
        }
    }

    // Also check properties for required markers
    if let Some(properties) = schema.get("properties").and_then(|v| v.as_object()) {
        for (key, value) in properties {
            if let Some(is_required) = value.get("required").and_then(|v| v.as_bool()) {
                if is_required && !required.contains(key) {
                    required.push(key.clone());
                }
            }
        }
    }

    required
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_extract_required_params() {
        let schema = json!({
            "type": "object",
            "properties": {
                "target": {"type": "string"},
                "port": {"type": "number"}
            },
            "required": ["target", "port"]
        });

        let required = extract_required_params(&schema);
        assert_eq!(required.len(), 2);
        assert!(required.contains(&"target".to_string()));
        assert!(required.contains(&"port".to_string()));
    }

    #[test]
    fn test_extract_required_params_empty() {
        let schema = json!({
            "type": "object",
            "properties": {
                "optional": {"type": "string"}
            }
        });

        let required = extract_required_params(&schema);
        assert_eq!(required.len(), 0);
    }

    #[test]
    fn test_extract_required_params_from_properties() {
        let schema = json!({
            "type": "object",
            "properties": {
                "target": {"type": "string", "required": true},
                "optional": {"type": "string", "required": false}
            }
        });

        let required = extract_required_params(&schema);
        assert_eq!(required.len(), 1);
        assert!(required.contains(&"target".to_string()));
    }

    #[test]
    fn test_parameter_compatibility_full() {
        let schema = json!({
            "required": ["target", "port"]
        });

        let mut available = HashMap::new();
        available.insert("target".to_string(), "example.com".to_string());
        available.insert("port".to_string(), "443".to_string());

        let compatibility = calculate_parameter_compatibility(&schema, &available);
        assert_eq!(compatibility, 1.0);
    }

    #[test]
    fn test_parameter_compatibility_partial() {
        let schema = json!({
            "required": ["target", "port"]
        });

        let mut available = HashMap::new();
        available.insert("target".to_string(), "example.com".to_string());

        let compatibility = calculate_parameter_compatibility(&schema, &available);
        assert_eq!(compatibility, 0.5);
    }

    #[test]
    fn test_parameter_compatibility_none() {
        let schema = json!({
            "required": ["target", "port"]
        });

        let available = HashMap::new();

        let compatibility = calculate_parameter_compatibility(&schema, &available);
        assert_eq!(compatibility, 0.0);
    }

    #[test]
    fn test_parameter_compatibility_no_required() {
        let schema = json!({
            "properties": {
                "optional": {"type": "string"}
            }
        });

        let available = HashMap::new();

        let compatibility = calculate_parameter_compatibility(&schema, &available);
        assert_eq!(compatibility, 1.0); // No required = fully compatible
    }
}
