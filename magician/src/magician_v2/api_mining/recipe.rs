//! Cross-origin, task-shaped API workflows compiled from a browser run.
//!
//! A recipe is keyed by task shape rather than origin. The existing
//! [`super::workflow::WorkflowGraph`] remains the per-origin mid-run artifact.

use super::capability::SideEffects;
use super::workflow::{BrowserFallbackStep, InferenceMethod, ReplayStats};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub use super::workflow::WorkflowMaturity as RecipeMaturity;

/// The request's declared representation controls parsing, not the presence
/// of an `=` inside arbitrary text/XML/GraphQL. Missing headers retain the
/// legacy JSON/form inference used by older stored recipes and trace fixtures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RequestBodyFormat {
    Json,
    Form,
    Opaque,
    Infer,
}

pub(crate) fn request_body_format(content_type: Option<&str>) -> RequestBodyFormat {
    let Some(media_type) = content_type.map(|value| value.split(';').next().unwrap_or("").trim())
    else {
        return RequestBodyFormat::Infer;
    };
    if media_type.eq_ignore_ascii_case("application/json")
        || media_type
            .to_ascii_lowercase()
            .strip_prefix("application/")
            .is_some_and(|subtype| subtype.ends_with("+json"))
    {
        RequestBodyFormat::Json
    } else if media_type.eq_ignore_ascii_case("application/x-www-form-urlencoded") {
        RequestBodyFormat::Form
    } else {
        RequestBodyFormat::Opaque
    }
}

/// Whether `body` is really a urlencoded form, whatever its Content-Type
/// claims. A well-known search client posts raw JSON under
/// `application/x-www-form-urlencoded` to avoid a CORS preflight; parsing that
/// as pairs yields one "key" holding the whole document, and re-serialising it
/// percent-encodes the body into something the server rejects outright
/// (measured live: 400 "Expecting object or array"). A real form has `=` and
/// field-shaped names.
pub(crate) fn is_urlencoded_form_body(body: &str) -> bool {
    if !body.contains('=') {
        return false;
    }
    let mut pairs = url::form_urlencoded::parse(body.as_bytes()).peekable();
    if pairs.peek().is_none() {
        return false;
    }
    url::form_urlencoded::parse(body.as_bytes()).all(|(key, _)| {
        !key.is_empty()
            && key.chars().all(|character| {
                character.is_ascii_alphanumeric()
                    || matches!(character, '_' | '-' | '.' | '[' | ']' | '+' | '{' | '}')
            })
    })
}

/// Mined text extractors identify one value, not an arbitrary first occurrence.
/// A second match means the capture/replay no longer proves which value to use.
/// Callers bound the response and pattern sizes; only two matches are examined.
pub(crate) fn extract_unique_regex<'text>(
    text: &'text str,
    pattern: &str,
    group: usize,
) -> Option<&'text str> {
    let expression = regex::Regex::new(pattern).ok()?;
    let mut matches = expression.captures_iter(text);
    let value = matches.next()?.get(group)?.as_str().trim();
    matches.next().is_none().then_some(value)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecipe {
    pub id: String,
    pub scope_principal: String,
    pub scope_workspace: String,
    pub agent_id: String,
    pub shape: RecipeShape,
    pub current_version: u32,
    pub versions: Vec<RecipeVersion>,
}

impl TaskRecipe {
    pub fn current(&self) -> Option<&RecipeVersion> {
        self.versions
            .iter()
            .find(|version| version.version == self.current_version)
    }

    pub fn current_mut(&mut self) -> Option<&mut RecipeVersion> {
        let current = self.current_version;
        self.versions
            .iter_mut()
            .find(|version| version.version == current)
    }

    pub fn has_write_steps(&self) -> bool {
        self.current().is_some_and(|version| {
            version
                .steps
                .iter()
                .any(|step| step.side_effects == SideEffects::Write)
        })
    }

    /// True only for a structurally runnable current version whose every step
    /// is explicitly classified read-only. Empty or unknown versions must not
    /// enter fuzzy matching, verification, or read-only planner lanes.
    pub fn is_read_only(&self) -> bool {
        self.current().is_some_and(|version| {
            !version.steps.is_empty()
                && version
                    .steps
                    .iter()
                    .all(|step| step.side_effects == SideEffects::ReadOnly)
        })
    }
}

impl RecipeStep {
    /// Recompute rather than trusting the serialized field so recipes written
    /// by an older fingerprint implementation cannot retain a broader write
    /// grant after the request-shape contract is tightened.
    pub fn effective_request_shape_fingerprint(&self) -> String {
        request_shape_fingerprint_with_headers(
            &self.method,
            &self.url_template,
            &self.headers_template,
            self.body_template.as_deref(),
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeShape {
    /// Intent template with named placeholders, such as `price of {product}`.
    pub template: String,
    /// The description is execution intent too. Older recipes without this
    /// proof may only match their unchanged source task, not a title alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_template: Option<String>,
    /// Blake3 over the template, agent id, and scope.
    pub fingerprint: String,
    #[serde(default)]
    pub inputs: Vec<TaskInput>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskInput {
    pub name: String,
    pub schema: TaskInputSchema,
    pub example_value: String,
    pub source: TaskInputSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskInputSchema {
    String,
    Number,
    Boolean,
}

impl TaskInputSchema {
    /// Match the scalar representation the request renderer can send. Rust's
    /// floating-point parser also accepts non-JSON forms such as `+42` and
    /// `.5`; accepting those here can fail a later body after an earlier write.
    pub(crate) fn accepts(self, value: &str) -> bool {
        match self {
            Self::String => true,
            Self::Number => parse_task_number(value).is_some(),
            Self::Boolean => {
                value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("false")
            },
        }
    }
}

pub(crate) fn parse_task_number(value: &str) -> Option<serde_json::Number> {
    let number = value.parse::<serde_json::Number>().ok()?;
    number
        .as_f64()
        .is_some_and(f64::is_finite)
        .then_some(number)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskInputSource {
    TaskText,
    BrowserTyped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeVersion {
    pub version: u32,
    pub origins: Vec<String>,
    pub steps: Vec<RecipeStep>,
    #[serde(default)]
    pub data_flows: Vec<RecipeDataFlow>,
    #[serde(default)]
    pub answer_spec: Vec<AnswerField>,
    #[serde(default)]
    pub auth: RecipeAuth,
    pub maturity: RecipeMaturity,
    #[serde(default)]
    pub replay_stats: ReplayStats,
    pub compiled_from: CompiledFrom,
    pub compiled_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_replayed_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeStep {
    pub id: String,
    pub origin: String,
    pub method: String,
    pub url_template: String,
    #[serde(default)]
    pub headers_template: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_id: Option<String>,
    #[serde(default)]
    pub param_sources: HashMap<String, RecipeParamSource>,
    /// Original JSON scalar types, used to restore unquoted body parameters.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub body_param_types: HashMap<String, TaskInputSchema>,
    pub side_effects: SideEffects,
    /// Method, URL template, and recursive body structure. Values are absent.
    pub request_shape_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_with: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_fallback: Option<BrowserFallbackStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_hint: Option<Transport>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecipeParamSource {
    Literal {
        value: String,
        #[serde(default)]
        volatile: bool,
    },
    DataFlow {
        flow_id: String,
    },
    TaskInput {
        name: String,
    },
    SessionAuth {
        scheme: String,
    },
    Now {
        unit: NowUnit,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NowUnit {
    Millis,
    Seconds,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeDataFlow {
    pub id: String,
    pub source_step: String,
    pub extractor: Extractor,
    pub target_step: String,
    pub target_param: String,
    pub confidence: f32,
    pub inference: InferenceMethod,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Extractor {
    JsonPath { path: String },
    Regex { pattern: String, group: usize },
    Header { name: String },
    Cookie { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnswerField {
    pub field: String,
    pub step_id: String,
    pub extractor: Extractor,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecipeAuth {
    #[serde(default)]
    pub requires_session: bool,
    #[serde(default)]
    pub origins_needing_auth: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_step_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledFrom {
    pub task_id: String,
    pub execution_id: String,
    /// Hash of the normalized source title + description. Exact task-id reuse
    /// may use compile-time input examples only while this still matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_text_fingerprint: Option<String>,
    /// Present only when this version was learned from a recurring Monitor.
    /// The optional background verifier uses this durable, typed provenance;
    /// it never guesses monitor ownership from titles, tags, or agent names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor_revision: Option<u32>,
    #[serde(default)]
    pub sequence_ids: Vec<String>,
    #[serde(default)]
    pub trace_files: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Reqwest,
    InPageFetch,
    Browser,
}

/// Stable fingerprint of a request shape. Dynamic values do not participate.
pub fn request_shape_fingerprint(
    method: &str,
    url_template: &str,
    body_template: Option<&str>,
) -> String {
    request_shape_fingerprint_parts(method, url_template, Vec::new(), body_template)
}

/// Recipe grants also bind the normalized header templates. Compiler output
/// has already replaced session secrets with named parameters, so hashing the
/// template detects behavior-bearing header changes without storing values in
/// the grant record.
pub fn request_shape_fingerprint_with_headers(
    method: &str,
    url_template: &str,
    headers_template: &HashMap<String, String>,
    body_template: Option<&str>,
) -> String {
    let headers = headers_template
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.clone()))
        .collect();
    let structural = request_shape_fingerprint_parts(method, url_template, headers, body_template);
    // Runtime inputs are placeholders here, whereas fixed body values can
    // select a wholly different operation (especially GraphQL mutations).
    // Bind those literals too; hashing retains no credential/body plaintext
    // in the grant and still permits new values for declared task inputs.
    let normalized_body = body_template.map(|body| {
        serde_json::from_str::<serde_json::Value>(body)
            .map(|value| value.to_string())
            .unwrap_or_else(|_| body.to_owned())
    });
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"task-recipe-grant.v5\0");
    hasher.update(structural.as_bytes());
    hasher.update(b"\0");
    if let Some(body) = normalized_body {
        hasher.update(body.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

fn request_shape_fingerprint_parts(
    method: &str,
    url_template: &str,
    mut headers: Vec<(String, String)>,
    body_template: Option<&str>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"task-recipe-request-shape.v4\0");
    hasher.update(method.to_ascii_uppercase().as_bytes());
    hasher.update(b"\0");
    hasher.update(url_template.as_bytes());
    hasher.update(b"\0");
    headers.sort_unstable();
    for (name, value_template) in headers {
        hasher.update(name.as_bytes());
        hasher.update(b"=");
        hasher.update(value_template.as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(b"\0");
    for key in request_body_shape(body_template) {
        hasher.update(key.as_bytes());
        hasher.update(b",");
    }
    hasher.finalize().to_hex().to_string()
}

/// Return a deterministic, value-free representation of the body structure
/// for grant previews and fingerprints. Nested JSON paths participate so a
/// grant for `{profile:{name}}` does not authorize `{profile:{role}}`.
pub fn request_body_shape(body: Option<&str>) -> Vec<String> {
    let Some(body) = body else {
        return Vec::new();
    };
    if body.len() > 256 * 1024 {
        return vec!["oversized".into()];
    }
    if let Ok(value @ (serde_json::Value::Object(_) | serde_json::Value::Array(_))) =
        serde_json::from_str::<serde_json::Value>(body)
    {
        let mut shape = Vec::new();
        let mut nodes = 0_usize;
        if !collect_json_body_shape(&value, "$", 0, &mut nodes, &mut shape) {
            return vec!["json:unsupported".into()];
        }
        shape.sort_unstable();
        shape.dedup();
        return shape;
    }

    if !body.contains('=') {
        return (!body.is_empty())
            .then(|| vec!["opaque".into()])
            .unwrap_or_default();
    }

    let mut keys = Vec::new();
    for (index, (key, _)) in url::form_urlencoded::parse(body.as_bytes()).enumerate() {
        if index >= 256 || key.is_empty() || key.len() > 128 {
            return vec!["form:unsupported".into()];
        }
        keys.push(format!("form:{key}"));
    }
    // Preserve duplicate keys: `item=a` and `item=a&item=b` are different
    // mutation shapes even though their set of field names is the same.
    keys.sort_unstable();
    if keys.is_empty() && !body.is_empty() {
        vec!["opaque".into()]
    } else {
        keys
    }
}

/// Durable write grants require a body shape whose complete structure can be
/// described without retaining values. Unsupported/opaque bodies may still
/// be approved once, but must ask again on every execution.
pub fn request_body_shape_is_grantable(body: Option<&str>) -> bool {
    request_body_shape(body).iter().all(|part| {
        !matches!(
            part.as_str(),
            "opaque" | "oversized" | "json:unsupported" | "form:unsupported"
        )
    })
}

fn collect_json_body_shape(
    value: &serde_json::Value,
    path: &str,
    depth: usize,
    nodes: &mut usize,
    output: &mut Vec<String>,
) -> bool {
    if depth > 32 || *nodes >= 4_096 {
        return false;
    }
    *nodes += 1;
    match value {
        serde_json::Value::Object(map) => {
            output.push(format!("object:{path}"));
            for (key, child) in map {
                if key.len() > 128 || path.len() > 4_096 {
                    return false;
                }
                let escaped = key.replace('~', "~0").replace('/', "~1");
                let child_path = format!("{path}/{escaped}");
                output.push(format!("key:{child_path}"));
                if !collect_json_body_shape(child, &child_path, depth + 1, nodes, output) {
                    return false;
                }
            }
        },
        serde_json::Value::Array(items) => {
            output.push(format!("array:{path}:len={}", items.len()));
            for (index, child) in items.iter().enumerate() {
                if !collect_json_body_shape(
                    child,
                    &format!("{path}/{index}"),
                    depth + 1,
                    nodes,
                    output,
                ) {
                    return false;
                }
            }
        },
        serde_json::Value::String(_) => output.push(format!("string:{path}")),
        serde_json::Value::Number(_) => output.push(format!("number:{path}")),
        serde_json::Value::Bool(_) => output.push(format!("boolean:{path}")),
        serde_json::Value::Null => output.push(format!("null:{path}")),
    }
    true
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn numeric_inputs_use_finite_json_syntax_without_rounding_integer_ids() {
        for value in ["+42", "01", "1.", ".5", "NaN", "inf", "1e999", "42x", " 42"] {
            assert!(!TaskInputSchema::Number.accepts(value), "{value}");
        }
        for value in ["0", "-0", "-42", "0.5", "1e+3", "18446744073709551615"] {
            assert!(TaskInputSchema::Number.accepts(value), "{value}");
        }
        assert_eq!(
            parse_task_number("18446744073709551615").unwrap().as_u64(),
            Some(u64::MAX)
        );
        assert!(TaskInputSchema::Boolean.accepts("TRUE"));
        assert!(!TaskInputSchema::Boolean.accepts("1"));
    }

    #[test]
    fn recipe_grants_bind_fixed_mutation_semantics_not_just_json_keys() {
        let fingerprint = |body| {
            request_shape_fingerprint_with_headers(
                "POST",
                "https://example.test/graphql",
                &HashMap::new(),
                Some(body),
            )
        };
        assert_ne!(
            fingerprint(r#"{"query":"mutation { addItem(id: $id) }","variables":{"id":"{id}"}}"#),
            fingerprint(
                r#"{"query":"mutation { deleteItem(id: $id) }","variables":{"id":"{id}"}}"#
            )
        );
        assert_ne!(
            fingerprint(r#"{"action":"add","id":"{id}"}"#),
            fingerprint(r#"{"action":"delete","id":"{id}"}"#)
        );
        assert_eq!(
            fingerprint(r#"{"action":"add", "id":"{id}"}"#),
            fingerprint(r#"{ "id":"{id}", "action":"add" }"#)
        );
    }

    fn sample_recipe() -> TaskRecipe {
        let steps = vec![
            RecipeStep {
                id: "s0".into(),
                origin: "https://hn.algolia.com".into(),
                method: "GET".into(),
                url_template: "https://hn.algolia.com/api/v1/search?query={query}&tags=story"
                    .into(),
                headers_template: HashMap::new(),
                body_template: None,
                capability_id: None,
                param_sources: HashMap::from([(
                    "query".to_owned(),
                    RecipeParamSource::TaskInput {
                        name: "query".into(),
                    },
                )]),
                body_param_types: HashMap::new(),
                side_effects: SideEffects::ReadOnly,
                request_shape_fingerprint: request_shape_fingerprint(
                    "GET",
                    "https://hn.algolia.com/api/v1/search?query={query}&tags=story",
                    None,
                ),
                verify_with: None,
                browser_fallback: None,
                transport_hint: None,
            },
            RecipeStep {
                id: "s1".into(),
                origin: "https://hn.algolia.com".into(),
                method: "GET".into(),
                url_template: "https://hn.algolia.com/api/v1/items/{item_id}".into(),
                headers_template: HashMap::new(),
                body_template: None,
                capability_id: None,
                param_sources: HashMap::from([(
                    "item_id".to_owned(),
                    RecipeParamSource::DataFlow {
                        flow_id: "df0".into(),
                    },
                )]),
                body_param_types: HashMap::new(),
                side_effects: SideEffects::ReadOnly,
                request_shape_fingerprint: request_shape_fingerprint(
                    "GET",
                    "https://hn.algolia.com/api/v1/items/{item_id}",
                    None,
                ),
                verify_with: None,
                browser_fallback: None,
                transport_hint: None,
            },
        ];
        TaskRecipe {
            id: "rcp_test".into(),
            scope_principal: "anonymous".into(),
            scope_workspace: "default".into(),
            agent_id: "personal-assistant".into(),
            shape: RecipeShape {
                description_template: None,
                template: "first hacker news story about {query}".into(),
                fingerprint: "fp".into(),
                inputs: vec![TaskInput {
                    name: "query".into(),
                    schema: TaskInputSchema::String,
                    example_value: "rust".into(),
                    source: TaskInputSource::TaskText,
                }],
            },
            current_version: 1,
            versions: vec![RecipeVersion {
                version: 1,
                origins: vec!["https://hn.algolia.com".into()],
                steps,
                data_flows: vec![RecipeDataFlow {
                    id: "df0".into(),
                    source_step: "s0".into(),
                    extractor: Extractor::JsonPath {
                        path: "$.hits[0].objectID".into(),
                    },
                    target_step: "s1".into(),
                    target_param: "item_id".into(),
                    confidence: 0.9,
                    inference: InferenceMethod::AutoMatch,
                }],
                answer_spec: vec![AnswerField {
                    field: "title".into(),
                    step_id: "s1".into(),
                    extractor: Extractor::JsonPath {
                        path: "$.title".into(),
                    },
                }],
                auth: RecipeAuth::default(),
                maturity: RecipeMaturity::Draft,
                replay_stats: ReplayStats::default(),
                compiled_from: CompiledFrom {
                    task_id: "task_1".into(),
                    execution_id: "exec_1".into(),
                    task_text_fingerprint: None,
                    monitor_revision: None,
                    sequence_ids: vec![],
                    trace_files: vec![],
                },
                compiled_at_ms: 1_780_000_000_000,
                last_replayed_at_ms: None,
            }],
        }
    }

    #[test]
    fn recipe_roundtrips_via_serde_and_exposes_current_version() {
        let json = serde_json::to_string(&sample_recipe()).unwrap();
        let recipe: TaskRecipe = serde_json::from_str(&json).unwrap();
        assert_eq!(recipe.current().unwrap().steps.len(), 2);
        assert!(!recipe.has_write_steps());
        assert!(recipe.is_read_only());
    }

    #[test]
    fn empty_and_unknown_versions_are_not_read_only() {
        let mut recipe = sample_recipe();
        recipe.current_mut().unwrap().steps.clear();
        assert!(!recipe.is_read_only());

        let mut recipe = sample_recipe();
        recipe.current_mut().unwrap().steps[0].side_effects = SideEffects::Unknown;
        assert!(!recipe.is_read_only());
    }

    #[test]
    fn legacy_recipe_without_monitor_provenance_remains_non_monitor_owned() {
        let mut value = serde_json::to_value(sample_recipe()).unwrap();
        value["versions"][0]["compiled_from"]
            .as_object_mut()
            .unwrap()
            .remove("monitor_revision");
        let recipe: TaskRecipe = serde_json::from_value(value).unwrap();
        assert_eq!(
            recipe.current().unwrap().compiled_from.monitor_revision,
            None
        );
    }

    #[test]
    fn request_shape_fingerprint_ignores_values_but_not_keys() {
        let a = request_shape_fingerprint(
            "POST",
            "https://x.test/api/cart",
            Some(r#"{"sku":"1","qty":2}"#),
        );
        let b = request_shape_fingerprint(
            "POST",
            "https://x.test/api/cart",
            Some(r#"{"sku":"999","qty":7}"#),
        );
        let c =
            request_shape_fingerprint("POST", "https://x.test/api/cart", Some(r#"{"sku":"1"}"#));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(
            a,
            request_shape_fingerprint(
                "PUT",
                "https://x.test/api/cart",
                Some(r#"{"sku":"1","qty":2}"#),
            )
        );
    }

    #[test]
    fn request_shape_fingerprint_covers_nested_json_without_values() {
        let a = request_shape_fingerprint(
            "PATCH",
            "https://x.test/api/profile",
            Some(r#"{"profile":{"name":"first"},"tags":[{"id":1}]}"#),
        );
        let same_shape = request_shape_fingerprint(
            "PATCH",
            "https://x.test/api/profile",
            Some(r#"{"profile":{"name":"second"},"tags":[{"id":999}]}"#),
        );
        let different_nested_key = request_shape_fingerprint(
            "PATCH",
            "https://x.test/api/profile",
            Some(r#"{"profile":{"role":"admin"},"tags":[{"id":1}]}"#),
        );

        assert_eq!(a, same_shape);
        assert_ne!(a, different_nested_key);
        assert!(
            request_body_shape(Some(r#"{"profile":{"name":"private"}}"#))
                .iter()
                .all(|part| !part.contains("private"))
        );
        assert!(request_body_shape_is_grantable(Some(
            r#"{"profile":{"name":"private"}}"#
        )));
        assert!(!request_body_shape_is_grantable(Some(
            "private opaque payload"
        )));
    }

    #[test]
    fn request_shape_fingerprint_preserves_array_cardinality_and_positions() {
        let one_item = request_shape_fingerprint(
            "POST",
            "https://x.test/api/cart",
            Some(r#"{"items":[{"sku":"one"}]}"#),
        );
        let two_items = request_shape_fingerprint(
            "POST",
            "https://x.test/api/cart",
            Some(r#"{"items":[{"sku":"one"},{"sku":"two"}]}"#),
        );
        let different_positions = request_shape_fingerprint(
            "POST",
            "https://x.test/api/cart",
            Some(r#"{"items":[{"sku":"one"},{"quantity":2}]}"#),
        );

        assert_ne!(one_item, two_items);
        assert_ne!(two_items, different_positions);
    }

    #[test]
    fn request_shape_fingerprint_preserves_repeated_form_key_count() {
        let once =
            request_shape_fingerprint("POST", "https://x.test/api/roles", Some("role=member"));
        let twice = request_shape_fingerprint(
            "POST",
            "https://x.test/api/roles",
            Some("role=member&role=admin"),
        );

        assert_ne!(once, twice);
        assert_eq!(
            request_body_shape(Some("role=member&role=admin")),
            vec!["form:role", "form:role"]
        );
    }

    #[test]
    fn effective_request_shape_fingerprint_binds_header_templates() {
        let mut step = sample_recipe().current().unwrap().steps[0].clone();
        let without_header = step.effective_request_shape_fingerprint();
        step.headers_template
            .insert("X-Operation".into(), "preview".into());
        let preview = step.effective_request_shape_fingerprint();
        step.headers_template
            .insert("x-operation".into(), "commit".into());
        step.headers_template.remove("X-Operation");
        let commit = step.effective_request_shape_fingerprint();

        assert_ne!(without_header, preview);
        assert_ne!(preview, commit);
    }

    #[test]
    fn form_body_shape_rejects_unbounded_or_invalid_key_sets() {
        let body = (0..257)
            .map(|index| format!("key{index}=private"))
            .collect::<Vec<_>>()
            .join("&");
        assert_eq!(request_body_shape(Some(&body)), vec!["form:unsupported"]);
        assert!(!request_body_shape_is_grantable(Some(&body)));
        assert!(!request_body_shape_is_grantable(Some(&format!(
            "{}=private",
            "x".repeat(129)
        ))));
    }

    #[test]
    fn extractor_tagged_union_roundtrips() {
        for extractor in [
            Extractor::JsonPath { path: "$.a".into() },
            Extractor::Regex {
                pattern: "id=(\\d+)".into(),
                group: 1,
            },
            Extractor::Header {
                name: "x-request-id".into(),
            },
            Extractor::Cookie {
                name: "csrf".into(),
            },
        ] {
            let json = serde_json::to_string(&extractor).unwrap();
            assert_eq!(serde_json::from_str::<Extractor>(&json).unwrap(), extractor);
        }
    }
}
