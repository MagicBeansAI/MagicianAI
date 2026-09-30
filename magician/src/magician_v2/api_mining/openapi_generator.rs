//! OpenAPI 3.0 spec generator from learned ApiCapabilities
//!
//! Converts a list of reverse-engineered API capabilities for a single origin
//! into a standards-compliant OpenAPI 3.0 JSON spec. The generated spec can be
//! consumed by Swagger UI, Redoc, or any OpenAPI-compatible tooling.

use indexmap::IndexMap;
use openapiv3::{
    Info, MediaType, OpenAPI, Operation, Parameter, ParameterData, ParameterSchemaOrContent,
    PathItem, PathStyle, Paths, ReferenceOr, RequestBody, Response, Responses, Schema, SchemaData,
    SchemaKind, Server, StatusCode, StringType, Type,
};
use regex::Regex;
use std::sync::LazyLock;

use super::capability::{ApiCapability, ConfidenceLevel, SideEffects};

/// Pre-compiled regex for extracting `{param}` placeholders from URL templates.
static PATH_PARAM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{([^}]+)\}").expect("valid regex"));

/// Generate an OpenAPI 3.0 spec from a list of capabilities for a single origin.
pub fn generate_openapi_spec(origin: &str, capabilities: &[ApiCapability]) -> OpenAPI {
    let mut paths_map: IndexMap<String, ReferenceOr<PathItem>> = IndexMap::new();

    for cap in capabilities {
        let path = extract_path(origin, &cap.url_template);
        let operation = build_operation(cap);

        let path_item = paths_map
            .entry(path)
            .or_insert_with(|| ReferenceOr::Item(PathItem::default()));

        if let ReferenceOr::Item(ref mut item) = path_item {
            set_operation_on_path_item(item, &cap.method, operation);
        }
    }

    OpenAPI {
        openapi: "3.0.3".to_string(),
        info: Info {
            title: format!("Learned API — {}", origin),
            description: Some(
                "Auto-generated OpenAPI spec from reverse-engineered API capabilities. \
                 Discovered by Magician's API mining pipeline."
                    .to_string(),
            ),
            version: "0.1.0".to_string(),
            ..Default::default()
        },
        servers: vec![Server {
            url: origin.to_string(),
            description: Some("Observed origin server".to_string()),
            ..Default::default()
        }],
        paths: Paths {
            paths: paths_map,
            extensions: IndexMap::new(),
        },
        ..Default::default()
    }
}

/// Strip the origin prefix from the url_template to get the path portion.
/// E.g. "https://api.example.com/v1/users/{id}" -> "/v1/users/{id}"
fn extract_path(origin: &str, url_template: &str) -> String {
    if let Some(stripped) = url_template.strip_prefix(origin) {
        if stripped.is_empty() {
            "/".to_string()
        } else if stripped.starts_with('/') {
            stripped.to_string()
        } else {
            format!("/{}", stripped)
        }
    } else {
        // Fallback: try to parse as URL and extract path+query
        if let Ok(url) = url::Url::parse(url_template) {
            let path = url.path().to_string();
            if path.is_empty() {
                "/".to_string()
            } else {
                path
            }
        } else {
            // Last resort: use the whole template
            format!("/{}", url_template)
        }
    }
}

/// Extract `{param}` placeholder names from a URL template.
fn extract_path_params(url_template: &str) -> Vec<String> {
    PATH_PARAM_RE
        .captures_iter(url_template)
        .map(|c| c[1].to_string())
        .collect()
}

/// Build an OpenAPI Operation from an ApiCapability.
fn build_operation(cap: &ApiCapability) -> Operation {
    let mut description_parts: Vec<String> = Vec::new();

    // GraphQL annotation
    if let Some(ref gql_op) = cap.graphql_operation {
        let kind_str = cap
            .graphql_operation_kind
            .map(|k| k.as_str().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        description_parts.push(format!("GraphQL {} `{}`", kind_str, gql_op));
    }

    // Side effects annotation
    let side_effect_label = match cap.side_effects {
        SideEffects::ReadOnly => "read-only",
        SideEffects::Write => "write (has side effects)",
        SideEffects::Unknown => "unknown side effects",
    };
    description_parts.push(format!("Side effects: {}", side_effect_label));

    // Auth requirements annotation
    let auth = &cap.auth_requirements;
    let mut auth_parts: Vec<String> = Vec::new();
    if !auth.cookies.is_empty() {
        auth_parts.push(format!("cookies: [{}]", auth.cookies.join(", ")));
    }
    if !auth.headers.is_empty() {
        auth_parts.push(format!("headers: [{}]", auth.headers.join(", ")));
    }
    if !auth.query_params.is_empty() {
        auth_parts.push(format!("query params: [{}]", auth.query_params.join(", ")));
    }
    if !auth.local_storage_keys.is_empty() {
        auth_parts.push(format!(
            "localStorage: [{}]",
            auth.local_storage_keys.join(", ")
        ));
    }
    if !auth.session_storage_keys.is_empty() {
        auth_parts.push(format!(
            "sessionStorage: [{}]",
            auth.session_storage_keys.join(", ")
        ));
    }
    if !auth_parts.is_empty() {
        description_parts.push(format!("Auth requirements: {}", auth_parts.join("; ")));
    }

    let description = if description_parts.is_empty() {
        None
    } else {
        Some(description_parts.join("\n\n"))
    };

    // Path parameters
    let params: Vec<ReferenceOr<Parameter>> = extract_path_params(&cap.url_template)
        .into_iter()
        .map(|name| {
            ReferenceOr::Item(Parameter::Path {
                parameter_data: ParameterData {
                    name,
                    description: None,
                    required: true,
                    deprecated: None,
                    format: ParameterSchemaOrContent::Schema(ReferenceOr::Item(Schema {
                        schema_data: SchemaData::default(),
                        schema_kind: SchemaKind::Type(Type::String(StringType::default())),
                    })),
                    example: None,
                    examples: IndexMap::new(),
                    explode: None,
                    extensions: IndexMap::new(),
                },
                style: PathStyle::Simple,
            })
        })
        .collect();

    // Request body
    let request_body = build_request_body(cap);

    // Extensions
    let mut extensions: IndexMap<String, serde_json::Value> = IndexMap::new();

    let confidence_str = match cap.confidence {
        ConfidenceLevel::Observed => "observed",
        ConfidenceLevel::Candidate => "candidate",
        ConfidenceLevel::Validated => "validated",
        ConfidenceLevel::Trusted => "trusted",
    };
    extensions.insert(
        "x-magician-confidence".to_string(),
        serde_json::Value::String(confidence_str.to_string()),
    );

    extensions.insert(
        "x-magician-sample-count".to_string(),
        serde_json::json!(cap.sample_count),
    );

    if cap.replay_success_count > 0 || cap.replay_failure_count > 0 {
        extensions.insert(
            "x-magician-replay-success".to_string(),
            serde_json::json!(cap.replay_success_count),
        );
        extensions.insert(
            "x-magician-replay-failure".to_string(),
            serde_json::json!(cap.replay_failure_count),
        );
    }

    // Default response
    let responses = Responses {
        default: Some(ReferenceOr::Item(Response {
            description: "Observed response (schema not captured)".to_string(),
            ..Default::default()
        })),
        responses: {
            let mut r = IndexMap::new();
            r.insert(
                StatusCode::Code(200),
                ReferenceOr::Item(Response {
                    description: "Successful response".to_string(),
                    ..Default::default()
                }),
            );
            r
        },
        extensions: IndexMap::new(),
    };

    Operation {
        summary: Some(cap.name.clone()),
        description,
        operation_id: Some(cap.id.clone()),
        parameters: params,
        request_body,
        responses,
        extensions,
        ..Default::default()
    }
}

/// Build request body from input_schema and body_template if present.
fn build_request_body(cap: &ApiCapability) -> Option<ReferenceOr<RequestBody>> {
    // Only attach request body for methods that typically have one
    let method_upper = cap.method.to_uppercase();
    if !matches!(method_upper.as_str(), "POST" | "PUT" | "PATCH" | "DELETE") {
        return None;
    }

    let has_schema = cap.input_schema.is_some();
    let has_body_template = cap.body_template.is_some();

    if !has_schema && !has_body_template {
        return None;
    }

    let mut content: IndexMap<String, MediaType> = IndexMap::new();

    let media_type = MediaType {
        schema: cap.input_schema.as_ref().and_then(|schema_val| {
            // Try to deserialize the serde_json::Value as an OpenAPI Schema.
            // If it fails, wrap the raw JSON value as a free-form object.
            serde_json::from_value::<Schema>(schema_val.clone())
                .ok()
                .map(ReferenceOr::Item)
        }),
        example: cap.body_template.as_ref().and_then(|tmpl| {
            // Try to parse body_template as JSON for the example value
            serde_json::from_str(tmpl).ok()
        }),
        ..Default::default()
    };

    content.insert("application/json".to_string(), media_type);

    Some(ReferenceOr::Item(RequestBody {
        description: Some("Request body (auto-discovered)".to_string()),
        content,
        required: true,
        extensions: IndexMap::new(),
    }))
}

/// Assign an operation to the appropriate method slot on a PathItem.
fn set_operation_on_path_item(item: &mut PathItem, method: &str, op: Operation) {
    match method.to_uppercase().as_str() {
        "GET" => item.get = Some(op),
        "POST" => item.post = Some(op),
        "PUT" => item.put = Some(op),
        "PATCH" => item.patch = Some(op),
        "DELETE" => item.delete = Some(op),
        "HEAD" => item.head = Some(op),
        "OPTIONS" => item.options = Some(op),
        "TRACE" => item.trace = Some(op),
        _ => {
            // Unknown method — store as GET with annotation
            item.get = Some(op);
        },
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::capability::{
        ApiCapability, AuthRequirements, ConfidenceLevel, GraphqlOperationKind,
    };

    fn make_capability(method: &str, url_template: &str, name: &str) -> ApiCapability {
        let mut cap = ApiCapability::new(
            name.to_string(),
            "https://api.example.com".to_string(),
            method.to_string(),
            url_template.to_string(),
        );
        cap.id = format!("cap-{}", name);
        cap
    }

    #[test]
    fn test_extract_path_strips_origin() {
        let path = extract_path(
            "https://api.example.com",
            "https://api.example.com/v1/users/{id}",
        );
        assert_eq!(path, "/v1/users/{id}");
    }

    #[test]
    fn test_extract_path_root() {
        let path = extract_path("https://api.example.com", "https://api.example.com");
        assert_eq!(path, "/");
    }

    #[test]
    fn test_extract_path_params() {
        let params = extract_path_params("https://api.example.com/users/{user_id}/posts/{post_id}");
        assert_eq!(params, vec!["user_id", "post_id"]);
    }

    #[test]
    fn test_extract_path_params_no_params() {
        let params = extract_path_params("https://api.example.com/users");
        assert!(params.is_empty());
    }

    #[test]
    fn test_generate_basic_spec() {
        let caps = vec![
            make_capability("GET", "https://api.example.com/v1/users", "list_users"),
            make_capability("GET", "https://api.example.com/v1/users/{id}", "get_user"),
        ];

        let spec = generate_openapi_spec("https://api.example.com", &caps);

        assert_eq!(spec.openapi, "3.0.3");
        assert_eq!(spec.info.title, "Learned API — https://api.example.com");
        assert_eq!(spec.servers.len(), 1);
        assert_eq!(spec.servers[0].url, "https://api.example.com");

        // Should have 2 paths
        assert_eq!(spec.paths.paths.len(), 2);

        // Check the user detail path has a path parameter
        let user_path = spec.paths.paths.get("/v1/users/{id}").unwrap();
        if let ReferenceOr::Item(item) = user_path {
            let op = item.get.as_ref().unwrap();
            assert_eq!(op.operation_id, Some("cap-get_user".to_string()));
            assert_eq!(op.parameters.len(), 1);
        } else {
            panic!("Expected Item, got Reference");
        }
    }

    #[test]
    fn test_generate_spec_with_post() {
        let mut cap = make_capability("POST", "https://api.example.com/v1/users", "create_user");
        cap.input_schema = Some(serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "email": { "type": "string" }
            }
        }));
        cap.body_template =
            Some(r#"{"name":"{{string:name}}","email":"{{string:email}}"}"#.to_string());

        let spec = generate_openapi_spec("https://api.example.com", &[cap]);
        let path = spec.paths.paths.get("/v1/users").unwrap();
        if let ReferenceOr::Item(item) = path {
            assert!(item.post.is_some());
            let op = item.post.as_ref().unwrap();
            assert!(op.request_body.is_some());
        } else {
            panic!("Expected Item");
        }
    }

    #[test]
    fn test_generate_spec_with_graphql() {
        let mut cap = make_capability("POST", "https://api.example.com/graphql", "get_user_gql");
        cap.graphql_operation = Some("GetUser".to_string());
        cap.graphql_operation_kind = Some(GraphqlOperationKind::Query);

        let spec = generate_openapi_spec("https://api.example.com", &[cap]);
        let path = spec.paths.paths.get("/graphql").unwrap();
        if let ReferenceOr::Item(item) = path {
            let op = item.post.as_ref().unwrap();
            let desc = op.description.as_ref().unwrap();
            assert!(desc.contains("GraphQL query `GetUser`"));
        } else {
            panic!("Expected Item");
        }
    }

    #[test]
    fn test_extensions_include_confidence() {
        let mut cap = make_capability("GET", "https://api.example.com/health", "health_check");
        cap.confidence = ConfidenceLevel::Trusted;
        cap.sample_count = 10;
        cap.replay_success_count = 5;
        cap.replay_failure_count = 1;

        let spec = generate_openapi_spec("https://api.example.com", &[cap]);
        let path = spec.paths.paths.get("/health").unwrap();
        if let ReferenceOr::Item(item) = path {
            let op = item.get.as_ref().unwrap();
            assert_eq!(
                op.extensions.get("x-magician-confidence"),
                Some(&serde_json::json!("trusted"))
            );
            assert_eq!(
                op.extensions.get("x-magician-sample-count"),
                Some(&serde_json::json!(10))
            );
            assert_eq!(
                op.extensions.get("x-magician-replay-success"),
                Some(&serde_json::json!(5))
            );
        } else {
            panic!("Expected Item");
        }
    }

    #[test]
    fn test_auth_requirements_in_description() {
        let mut cap = make_capability("GET", "https://api.example.com/data", "get_data");
        cap.auth_requirements = AuthRequirements {
            cookies: vec!["SID".to_string()],
            headers: vec!["authorization".to_string()],
            query_params: vec!["api_key".to_string()],
            local_storage_keys: vec![],
            session_storage_keys: vec![],
        };

        let spec = generate_openapi_spec("https://api.example.com", &[cap]);
        let path = spec.paths.paths.get("/data").unwrap();
        if let ReferenceOr::Item(item) = path {
            let op = item.get.as_ref().unwrap();
            let desc = op.description.as_ref().unwrap();
            assert!(desc.contains("cookies: [SID]"));
            assert!(desc.contains("headers: [authorization]"));
            assert!(desc.contains("query params: [api_key]"));
        } else {
            panic!("Expected Item");
        }
    }

    #[test]
    fn test_multiple_methods_same_path() {
        let caps = vec![
            make_capability("GET", "https://api.example.com/v1/items", "list_items"),
            make_capability("POST", "https://api.example.com/v1/items", "create_item"),
        ];

        let spec = generate_openapi_spec("https://api.example.com", &caps);
        assert_eq!(spec.paths.paths.len(), 1);

        let path = spec.paths.paths.get("/v1/items").unwrap();
        if let ReferenceOr::Item(item) = path {
            assert!(item.get.is_some());
            assert!(item.post.is_some());
        } else {
            panic!("Expected Item");
        }
    }
}
