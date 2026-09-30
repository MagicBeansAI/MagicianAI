//! Transport-independent adaptation of Magician human input to MCP form mode.
//! Only this module interprets submitted form content; transport acceptance is
//! never itself an approval. Credentials and rich review surfaces stay in the UI.

use std::collections::{BTreeMap, HashSet};

use serde_json::{json, Map, Value};
use std::sync::Arc;

use crate::magician_v2::execution::agentic::types::{
    ChoiceOption, FormAnswer, UserInputType, UserInputValue,
};

const MAX_FIELDS: usize = 32;
const MAX_OPTIONS: usize = 128;
const MAX_TEXT_BYTES: usize = 65_536;

#[derive(Clone)]
enum Field {
    Text,
    Boolean,
    Choice(Vec<ChoiceOption>),
    MultiChoice(Vec<ChoiceOption>, usize, usize),
}

impl Field {
    fn schema(&self) -> Value {
        match self {
            Self::Text => json!({"type": "string", "maxLength": MAX_TEXT_BYTES}),
            Self::Boolean => json!({"type": "boolean"}),
            Self::Choice(options) => json!({
                "type": "string",
                "oneOf": options.iter().map(|o| json!({"const": o.id, "title": o.label})).collect::<Vec<_>>()
            }),
            Self::MultiChoice(options, min, max) => {
                let mut schema = json!({
                    "type": "array", "minItems": min,
                    "items": {"anyOf": options.iter().map(|o| json!({"const": o.id, "title": o.label})).collect::<Vec<_>>()}
                });
                if *max > 0 {
                    schema["maxItems"] = json!(max);
                }
                schema
            },
        }
    }

    fn validate(&self, value: &Value) -> Result<(), String> {
        let valid = match self {
            Self::Text => value.as_str().is_some_and(|v| v.len() <= MAX_TEXT_BYTES),
            Self::Boolean => value.is_boolean(),
            Self::Choice(options) => value
                .as_str()
                .is_some_and(|v| options.iter().any(|o| o.id == v)),
            Self::MultiChoice(options, min, max) => value.as_array().is_some_and(|values| {
                let mut seen = HashSet::new();
                values.len() >= *min
                    && (*max == 0 || values.len() <= *max)
                    && values.iter().all(|v| {
                        v.as_str()
                            .is_some_and(|id| options.iter().any(|o| o.id == id) && seen.insert(id))
                    })
            }),
        };
        valid
            .then_some(())
            .ok_or_else(|| "answer does not match the requested field".to_string())
    }
}

fn check_options(options: &[ChoiceOption]) -> Result<(), String> {
    let mut seen = HashSet::new();
    if options.is_empty()
        || options.len() > MAX_OPTIONS
        || options.iter().any(|o| {
            o.id.is_empty() || o.id.len() > 256 || o.label.len() > 4096 || !seen.insert(&o.id)
        })
    {
        return Err("choices must have distinct nonempty IDs and between 1 and 128 options".into());
    }
    Ok(())
}

/// A schema and its exact source type travel together until the answer is decoded.
#[derive(Clone)]
pub struct InputForm {
    input_type: UserInputType,
    fields: BTreeMap<String, Field>,
    required: Vec<String>,
    schema: Value,
}

impl InputForm {
    pub fn new(input_type: UserInputType) -> Result<Self, String> {
        let mut fields = BTreeMap::new();
        let mut required = Vec::new();
        let mut titles = BTreeMap::new();
        match &input_type {
            UserInputType::Text { .. } | UserInputType::Guidance { .. } => {
                fields.insert("value".into(), Field::Text);
            },
            UserInputType::Confirmation { .. } => {
                fields.insert("confirm".into(), Field::Boolean);
            },
            UserInputType::Choice {
                options,
                allow_other,
            } => {
                check_options(options)?;
                let mut options = options.clone();
                if *allow_other {
                    if options.iter().any(|o| o.id == "other") {
                        return Err(
                            "the other option ID is reserved when allow_other is enabled".into(),
                        );
                    }
                    options.push(ChoiceOption {
                        id: "other".into(),
                        label: "Other".into(),
                        description: None,
                    });
                    fields.insert("other_value".into(), Field::Text);
                }
                fields.insert("selected_id".into(), Field::Choice(options));
            },
            UserInputType::MultiChoice {
                options,
                min_selections,
                max_selections,
            } => {
                check_options(options)?;
                if *min_selections > options.len()
                    || (*max_selections > 0 && min_selections > max_selections)
                {
                    return Err("invalid selection limits".into());
                }
                fields.insert(
                    "selected_ids".into(),
                    Field::MultiChoice(options.clone(), *min_selections, *max_selections),
                );
            },
            UserInputType::Form { questions } => {
                if questions.is_empty() || questions.len() > MAX_FIELDS {
                    return Err("a form requires between 1 and 32 questions".into());
                }
                for question in questions {
                    if question.id.is_empty()
                        || question.id.len() > 256
                        || fields.contains_key(&question.id)
                    {
                        return Err("form question IDs must be distinct and nonempty".into());
                    }
                    let field = match question.input_type.as_str() {
                        "text" => Field::Text,
                        "choice" => {
                            check_options(&question.options)?;
                            Field::Choice(question.options.clone())
                        },
                        "multi_choice" => {
                            check_options(&question.options)?;
                            Field::MultiChoice(question.options.clone(), 0, question.options.len())
                        },
                        "password" | "otp" => return Err(CREDENTIAL_REFUSAL.into()),
                        _ => {
                            return Err("form fields support text, choice, and multi_choice".into())
                        },
                    };
                    fields.insert(question.id.clone(), field);
                    titles.insert(question.id.clone(), question.prompt.clone());
                }
            },
            UserInputType::ExternalAction { instructions, .. } => {
                fields.insert("completed".into(), Field::Boolean);
                fields.insert("guidance".into(), Field::Text);
                titles.insert("completed".into(), instructions.clone());
            },
            UserInputType::FilePath {
                multiple: false, ..
            } => {
                fields.insert("path".into(), Field::Text);
            },
            UserInputType::ToolAuthorization { .. } | UserInputType::SandboxOverride { .. } => {
                // Match the existing resume owner's decision vocabulary. Never
                // invent a broader allow-always option for sandbox overrides.
                let options = serde_json::from_str::<Vec<ChoiceOption>>(
                    &input_type.options_json().unwrap_or_default(),
                )
                .map_err(|_| "invalid authorization options".to_string())?;
                fields.insert("selected_id".into(), Field::Choice(options));
            },
            UserInputType::Password { .. } | UserInputType::Otp { .. } => {
                return Err(CREDENTIAL_REFUSAL.into())
            },
            UserInputType::DiffApproval { .. } => {
                return Err("diff review requires Magician's review UI".into())
            },
            UserInputType::FilePath { multiple: true, .. } => {
                return Err("multiple file paths require Magician's UI".into())
            },
        }
        // Magician forms permit skipping individual questions. Omitted fields
        // decode as skipped; an empty string and an empty selection stay answers.
        if !matches!(input_type, UserInputType::Form { .. }) {
            required.extend(
                fields
                    .keys()
                    .filter(|id| !matches!(id.as_str(), "other_value" | "guidance"))
                    .cloned(),
            );
        }
        let properties: Map<String, Value> = fields
            .iter()
            .map(|(id, field)| {
                let mut schema = field.schema();
                if let Some(title) = titles.get(id) {
                    schema["title"] = json!(title);
                }
                (id.clone(), schema)
            })
            .collect();
        let schema = json!({"type": "object", "properties": properties, "required": required});
        Ok(Self {
            input_type,
            fields,
            required,
            schema,
        })
    }

    pub fn schema(&self) -> &Value {
        &self.schema
    }

    pub fn decode(&self, result: &Value) -> Result<UserInputValue, String> {
        match result.get("action").and_then(Value::as_str) {
            Some(action @ ("decline" | "cancel")) => {
                return Ok(UserInputValue::aborted(Some(action.into())))
            },
            Some("accept") => {},
            _ => return Err("elicitation result requires accept, decline, or cancel".into()),
        }
        let content = result
            .get("content")
            .and_then(Value::as_object)
            .ok_or_else(|| "accepted forms require an object content".to_string())?;
        for key in &self.required {
            if !content.contains_key(key) {
                return Err(format!("missing required field: {key}"));
            }
        }
        for (key, value) in content {
            self.fields
                .get(key)
                .ok_or_else(|| "unexpected answer field".to_string())?
                .validate(value)?;
        }
        let string = |key: &str| {
            content
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let strings = |key: &str| {
            content
                .get(key)
                .and_then(Value::as_array)
                .map(|vs| {
                    vs.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let boolean = |key: &str| content.get(key).and_then(Value::as_bool).unwrap_or(false);
        Ok(match &self.input_type {
            UserInputType::Text { .. } => UserInputValue::text(string("value")),
            UserInputType::Guidance { .. } => UserInputValue::Guidance {
                advice: string("value"),
            },
            UserInputType::Confirmation { .. } => UserInputValue::confirmed(boolean("confirm")),
            UserInputType::Choice { allow_other, .. } => {
                let selected_id = string("selected_id");
                let other_value = content
                    .get("other_value")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if selected_id == "other"
                    && *allow_other
                    && other_value.as_deref().is_none_or(|v| v.trim().is_empty())
                {
                    return Err("Other requires a nonempty other_value".into());
                }
                if (selected_id != "other" || !allow_other) && other_value.is_some() {
                    return Err("other_value is only valid for the Other selection".into());
                }
                UserInputValue::Choice {
                    selected_id,
                    other_value,
                }
            },
            UserInputType::ToolAuthorization { .. } | UserInputType::SandboxOverride { .. } => {
                UserInputValue::Choice {
                    selected_id: string("selected_id"),
                    other_value: None,
                }
            },
            UserInputType::MultiChoice { .. } => UserInputValue::MultiChoice {
                selected_ids: strings("selected_ids"),
            },
            UserInputType::Form { questions } => UserInputValue::Form {
                answers: questions
                    .iter()
                    .map(|q| FormAnswer {
                        id: q.id.clone(),
                        skipped: !content.contains_key(&q.id),
                        value: content
                            .get(&q.id)
                            .and_then(Value::as_str)
                            .filter(|_| q.input_type == "text")
                            .map(str::to_string),
                        selected_ids: match q.input_type.as_str() {
                            "choice" => content
                                .get(&q.id)
                                .and_then(Value::as_str)
                                .map(|s| vec![s.to_string()])
                                .unwrap_or_default(),
                            "multi_choice" => strings(&q.id),
                            _ => Vec::new(),
                        },
                    })
                    .collect(),
            },
            UserInputType::ExternalAction { .. } if boolean("completed") => {
                UserInputValue::ExternalActionCompleted {
                    guidance: content
                        .get("guidance")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                }
            },
            UserInputType::ExternalAction { .. } => {
                UserInputValue::aborted(Some("external action not completed".into()))
            },
            UserInputType::FilePath { .. } => UserInputValue::FilePath {
                paths: vec![string("path")],
            },
            _ => return Err("unsupported input type".into()),
        })
    }
}

/// Parse the existing need_user_input argument vocabulary, also used by service
/// requests carrying `context.input_schema`. No execution authority is inferred.
pub fn input_type_from_arguments(arguments: &Value) -> Result<UserInputType, String> {
    let mut fields = arguments
        .as_object()
        .cloned()
        .ok_or("input arguments must be an object")?;
    let mut kind = match fields.remove("input_type") {
        None => "text".into(),
        Some(Value::String(kind)) => kind.to_lowercase(),
        Some(_) => return Err("input_type must be a string".into()),
    };
    if let Some(questions) = fields.get_mut("questions").and_then(Value::as_array_mut) {
        if questions.len() >= 2 {
            kind = "form".into();
        }
        for question in questions {
            if question.get("prompt").is_none() {
                if let Some(prompt) = question.get("question").cloned() {
                    question["prompt"] = prompt;
                }
            }
        }
    }
    fields.insert("type".into(), json!(kind));
    if kind == "multi_choice" {
        fields.entry("min_selections").or_insert(json!(0));
        fields.entry("max_selections").or_insert(json!(0));
    }
    serde_json::from_value(Value::Object(fields))
        .map_err(|_| "invalid user input specification".into())
}

/// Exact revision of a pause's question and captured action. Reusing a storage
/// key for the next pause must not let an old form answer resume that new pause.
pub fn pause_input_revision(
    paused_at: &chrono::DateTime<chrono::Utc>,
    input_type: &UserInputType,
    question: Option<&str>,
    confirmation_action_json: Option<&str>,
) -> String {
    blake3::hash(
        json!([paused_at, input_type, question, confirmation_action_json])
            .to_string()
            .as_bytes(),
    )
    .to_hex()
    .to_string()
}

/// An optional route carried only by the currently executing MCP call. Spawned
/// background tasks do not inherit it; delegated runs use an explicit wait call.
#[async_trait::async_trait]
pub trait InputChannel: Send + Sync {
    async fn ask(
        &self,
        question: &str,
        input_type: UserInputType,
    ) -> Result<UserInputValue, String>;
}

#[derive(Clone)]
pub struct InputRoute {
    pub principal: String,
    pub workspace: String,
    pub channel: Arc<dyn InputChannel>,
}

tokio::task_local! { pub static INPUT_ROUTE: InputRoute; }

/// Why the plane never carries a credential: the typed refusal in
/// [`InputForm::new`] and the spec refusals below all say the same thing.
pub const CREDENTIAL_REFUSAL: &str =
    "sensitive input requires Magician's authenticated UI; credentials cannot be collected through MCP forms";

/// Adapt the existing service request vocabulary without interpreting arbitrary
/// authorization sources as plain text questions.
///
/// A request the service classified sensitive is refused whatever its typed
/// shape — a `text` ask whose wording names a code is a credential ask. The
/// spec is the authority (P3); the typed refusal in [`InputForm::new`] is the
/// belt for a request restored from before the spec existed.
pub fn service_input_type(
    request: &crate::magician_v2::user_requests::UserRequest,
) -> Result<UserInputType, String> {
    if request.sensitive.is_some() {
        return Err(CREDENTIAL_REFUSAL.into());
    }
    if matches!(
        request.request_type.as_str(),
        "need_user_input" | "user_input"
    ) {
        if let Some(input_type) = request.context.get("input_type").filter(|v| v.is_object()) {
            return serde_json::from_value(input_type.clone())
                .map_err(|_| "invalid typed user input specification".into());
        }
        if request.options.iter().any(|option| option.requires_input) {
            return Err("this conditional input requires Magician's UI".into());
        }
        let mut args = request
            .context
            .get("input_schema")
            .cloned()
            .unwrap_or_else(|| request.context.clone());
        if !args.is_object() {
            return Err("invalid user input specification: expected an object".into());
        }
        if args.get("input_type").is_none() {
            args["input_type"] = request
                .context
                .get("input_type")
                .cloned()
                .unwrap_or(json!("text"));
        }
        if args.get("options").is_none() && !request.options.is_empty() {
            args["options"] = json!(request.options);
        }
        return input_type_from_arguments(&args);
    }
    if request.request_type == "confirmation" {
        return Ok(UserInputType::Confirmation {
            confirm_label: None,
            deny_label: None,
            destructive: false,
        });
    }
    if matches!(
        request.request_type.as_str(),
        "tool_authorization" | "sandbox_override" | "cannot_proceed"
    ) && !request.options.is_empty()
    {
        if request.options.iter().any(|o| o.requires_input) {
            return Err("this authorization requires Magician's review UI".into());
        }
        return Ok(UserInputType::Choice {
            options: request
                .options
                .iter()
                .map(|o| ChoiceOption {
                    id: o.id.clone(),
                    label: o.label.clone(),
                    description: None,
                })
                .collect(),
            allow_other: false,
        });
    }
    Err("this input request requires Magician's UI".into())
}

/// Preserve the service's existing decision/input contract. Typed runtime
/// pauses bypass this compatibility conversion and resume with UserInputValue.
pub fn service_response(
    request: &crate::magician_v2::user_requests::UserRequest,
    value: UserInputValue,
) -> crate::magician_v2::user_requests::UserResponse {
    use crate::magician_v2::secrets::classify::is_secret_param_name;
    use crate::magician_v2::user_requests::UserResponse;
    // A form answering a request whose spec flags fields ships the whole form
    // as one JSON object: the service deposits the flagged fields into
    // custody and keeps the ordinary ones usable as text. A form with no
    // such spec renders as text, withholding any field whose id reads as a
    // secret (compatibility detection; it can only withhold more).
    let form_ships_json = request
        .sensitive
        .as_ref()
        .is_some_and(|spec| !spec.fields.is_empty());
    let request_id = request.id.clone();
    let (decision, input) = match value {
        UserInputValue::Text { value } => ("provide_input".into(), Some(value)),
        UserInputValue::Guidance { advice } => ("provide_input".into(), Some(advice)),
        UserInputValue::Choice {
            selected_id,
            other_value,
        } => (selected_id, other_value),
        UserInputValue::MultiChoice { selected_ids } => {
            (selected_ids.join(","), Some(selected_ids.join(",")))
        },
        UserInputValue::Confirmation { confirmed } => {
            ((if confirmed { "confirm" } else { "deny" }).into(), None)
        },
        UserInputValue::Aborted { reason } => ("cancel".into(), reason),
        UserInputValue::Form { answers } if form_ships_json => {
            let object: Map<String, Value> = answers
                .iter()
                .filter(|answer| !answer.skipped)
                .map(|answer| {
                    let value = if !answer.selected_ids.is_empty() {
                        answer.selected_ids.join(", ")
                    } else {
                        answer.value.clone().unwrap_or_default()
                    };
                    (answer.id.clone(), Value::String(value))
                })
                .collect();
            (
                "provide_input".into(),
                Some(Value::Object(object).to_string()),
            )
        },
        UserInputValue::Form { answers } => (
            "provide_input".into(),
            Some(
                answers
                    .iter()
                    .map(|answer| {
                        let value = if answer.skipped {
                            "(skipped)".into()
                        } else if is_secret_param_name(&answer.id) {
                            "[sensitive; withheld]".into()
                        } else if !answer.selected_ids.is_empty() {
                            answer.selected_ids.join(", ")
                        } else {
                            answer.value.clone().unwrap_or_default()
                        };
                        format!("{}: {value}", answer.id)
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        ),
        UserInputValue::ExternalActionCompleted { guidance } => ("provide_input".into(), guidance),
        UserInputValue::FilePath { paths } => ("provide_input".into(), Some(paths.join(","))),
        UserInputValue::Password { .. } => ("cancel".into(), None),
    };
    UserResponse {
        request_id,
        decision,
        input,
        channel: "mcp".into(),
        sensitive: Vec::new(),
    }
}

/// The typed shape of an agentic pause the plane may ask, or the credential
/// refusal when the pause decided a sensitivity spec (the same verdict its
/// announcement published and every client masks by).
pub fn pause_input_type(
    pause: &crate::magician_v2::execution::agentic::PendingPauseInfo,
) -> Result<UserInputType, String> {
    if pause.pending_sensitive.is_some() {
        return Err(CREDENTIAL_REFUSAL.into());
    }
    Ok(pause.input_type.clone())
}

#[cfg(test)]
mod sensitive_response_tests {
    use super::*;
    use crate::magician_v2::execution::agentic::types::FormAnswer;
    use crate::magician_v2::user_requests::{
        SensitiveField, SensitiveInputSpec, SensitiveKind, SensitiveProvenance, UserRequest,
    };

    #[test]
    fn the_plane_refuses_a_one_time_code_like_a_password() {
        use crate::magician_v2::execution::agentic::types::FormQuestion;
        // P3 Task 3.4: an MCP form never collects a credential — typed at the
        // top level or as a form field.
        for input_type in [
            UserInputType::Password { placeholder: None },
            UserInputType::Otp { placeholder: None },
            UserInputType::Form {
                questions: vec![FormQuestion {
                    id: "code".into(),
                    prompt: "Code".into(),
                    input_type: "otp".into(),
                    options: vec![],
                }],
            },
        ] {
            let Err(error) = InputForm::new(input_type) else {
                panic!("a credential ask must be refused on the plane");
            };
            assert!(
                error.contains("credentials cannot be collected through MCP forms"),
                "{error}"
            );
        }
    }

    #[test]
    fn the_plane_refuses_a_spec_classified_ask_before_its_form() {
        // Reviewer finding at the P3 gate: a `text` ask the service classified
        // as a code (wording) must not be elicited through an MCP client —
        // the spec decides, not the typed shape.
        let mut asked = request(Some(SensitiveInputSpec {
            kind: Some(SensitiveKind::Otp),
            fields: vec![],
            provenance: SensitiveProvenance::Heuristic,
            one_time: true,
            collection_deadline_ms: i64::MAX,
            challenge_id: None,
            revision: 0,
            expected_destination: None,
        }));
        asked.context = json!({ "input_type": "text" });
        let Err(error) = service_input_type(&asked) else {
            panic!("a spec-classified ask must be refused on the plane");
        };
        assert_eq!(error, CREDENTIAL_REFUSAL);
        // The same request without a spec is an ordinary text ask.
        let mut plain = request(None);
        plain.context = json!({ "input_type": "text" });
        assert!(matches!(
            service_input_type(&plain),
            Ok(UserInputType::Text { .. })
        ));

        // An agentic pause carries its own verdict on the listing.
        let mut pause = crate::magician_v2::execution::agentic::PendingPauseInfo::for_test(
            "pause-1",
            "exec-1",
            UserInputType::Text {
                placeholder: None,
                multiline: false,
            },
        );
        assert!(pause_input_type(&pause).is_ok());
        pause.pending_sensitive = Some(login_bundle());
        assert_eq!(
            pause_input_type(&pause),
            Err(CREDENTIAL_REFUSAL.to_string())
        );
    }

    fn request(sensitive: Option<SensitiveInputSpec>) -> UserRequest {
        UserRequest {
            id: "req-plane-1".into(),
            request_type: "need_user_input".into(),
            question: "Log in".into(),
            options: vec![],
            principal: "owner".into(),
            workspace: "workspace".into(),
            context: json!({}),
            source: "plane".into(),
            execution_id: None,
            task_id: None,
            timeout_secs: 60,
            default_on_timeout: "timeout".into(),
            created_at: 1,
            sensitive,
        }
    }

    fn login_bundle() -> SensitiveInputSpec {
        SensitiveInputSpec {
            kind: None,
            fields: vec![
                SensitiveField {
                    id: "email".into(),
                    kind: SensitiveKind::LoginIdentifier,
                },
                SensitiveField {
                    id: "pw".into(),
                    kind: SensitiveKind::Password,
                },
            ],
            provenance: SensitiveProvenance::FormSchema,
            one_time: false,
            collection_deadline_ms: i64::MAX,
            challenge_id: None,
            revision: 0,
            expected_destination: None,
        }
    }

    fn answer(id: &str, value: &str) -> FormAnswer {
        FormAnswer {
            id: id.into(),
            skipped: false,
            value: Some(value.into()),
            selected_ids: vec![],
        }
    }

    #[test]
    fn a_spec_form_ships_every_field_as_one_json_object_for_the_service_to_split() {
        let value = UserInputValue::Form {
            answers: vec![
                answer("email", "plane-id-canary-17"),
                answer("pw", "plane-pw-canary-18"),
                answer("remember", "yes"),
            ],
        };
        let response = service_response(&request(Some(login_bundle())), value);
        assert_eq!(response.decision, "provide_input");
        let shipped: Value =
            serde_json::from_str(response.input.as_deref().expect("json transport")).unwrap();
        assert_eq!(shipped["email"], json!("plane-id-canary-17"));
        assert_eq!(shipped["pw"], json!("plane-pw-canary-18"));
        assert_eq!(shipped["remember"], json!("yes"));
    }

    #[test]
    fn a_form_without_a_spec_renders_text_but_withholds_a_secret_named_field() {
        let value = UserInputValue::Form {
            answers: vec![
                answer("city", "Lisbon"),
                answer("password", "plane-compat-canary-19"),
            ],
        };
        let response = service_response(&request(None), value);
        let text = response.input.unwrap();
        assert!(text.contains("city: Lisbon"), "{text}");
        assert!(!text.contains("plane-compat-canary-19"), "{text}");
    }

    #[test]
    fn a_password_value_is_still_cancelled_on_the_plane() {
        let response = service_response(
            &request(Some(login_bundle())),
            UserInputValue::Password {
                value: "plane-pw-canary-20".into(),
            },
        );
        assert_eq!(response.decision, "cancel");
        assert_eq!(response.input, None);
    }

    #[test]
    fn a_text_answer_to_a_spec_request_is_passed_through_for_custody() {
        let mut spec = login_bundle();
        spec.fields.clear();
        spec.kind = Some(SensitiveKind::Otp);
        let response = service_response(
            &request(Some(spec)),
            UserInputValue::Text {
                value: "123456".into(),
            },
        );
        assert_eq!(response.decision, "provide_input");
        assert_eq!(response.input.as_deref(), Some("123456"));
    }
}
