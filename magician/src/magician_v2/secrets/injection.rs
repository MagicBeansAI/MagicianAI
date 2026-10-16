//! Magician-owned action traversal over shared credential helpers.
//! Existing execution owners call these functions directly; no daemon or new
//! model-facing secure_* operation is introduced by the extraction. Since P4
//! a user-typed reference is lowered only through `secrets::sinks` (approved
//! sinks, one-time material at dispatch); this module keeps the provisioned
//! injection targets, the unresolved-reference guard and result sanitisation.
use super::{cookie_field_value, CookieSpec, InjectionTarget};
use crate::magician_v2::execution::capability::{CommandArgMapping, ImplementationType};
use crate::magician_v2::execution::{ActionResult, ExecutableAction, FileAction, HttpAction};
use serde_json::{Map, Value};
use std::collections::HashMap;

pub use magicvault_core::injection::{
    cookie_domain_matches_host, detect_jwt_expiry, filter_cookies_for_url,
    known_value_replacements, resolve_inline_placeholders, sanitize_json_for_provider,
    sanitize_json_for_provider_owned, sanitize_text_for_provider, warm_provider_sanitizer,
    CookieWithMetadata, InjectionError, KnownSecretValues, SameSite,
};
use magicvault_core::injection::{
    replace_known_values, sanitize_json_value, REDACTED_PREFIX, REF_PREFIX,
};

/// Inject a provisioned or captured secret into an outbound action.
pub fn inject(
    action: ExecutableAction,
    fields: &HashMap<String, String>,
    target: &InjectionTarget,
) -> Result<ExecutableAction, InjectionError> {
    match target {
        InjectionTarget::Header { name, prefix } => inject_header(action, fields, name, prefix),
        InjectionTarget::FormFields(mapping) => inject_form_fields(action, fields, mapping),
        InjectionTarget::Cookies(specs) => inject_cookies(action, fields, specs),
        InjectionTarget::Inline => Err(InjectionError::InlineRequiresStore),
    }
}

fn implementation_has_unresolved_refs(
    implementation: &ImplementationType,
    text_has_ref: impl Fn(&str) -> bool + Copy,
) -> bool {
    let strings_have_ref = |values: &[String]| values.iter().any(|value| text_has_ref(value));
    let map_has_ref = |values: &HashMap<String, String>| {
        values
            .iter()
            .any(|(key, value)| text_has_ref(key) || text_has_ref(value))
    };
    let mapping_has_ref = |mapping: &CommandArgMapping| match mapping {
        CommandArgMapping::Positional { param }
        | CommandArgMapping::SplitPositional { param }
        | CommandArgMapping::Passthrough { param } => text_has_ref(param),
        CommandArgMapping::Flag { flag, param } | CommandArgMapping::BoolFlag { flag, param } => {
            text_has_ref(flag) || text_has_ref(param)
        },
        CommandArgMapping::EnvFlag { flag, env_var } => text_has_ref(flag) || text_has_ref(env_var),
        CommandArgMapping::FixedArgs { args } => strings_have_ref(args),
    };

    match implementation {
        ImplementationType::Composite { steps } => steps.iter().any(|step| {
            text_has_ref(&step.tool)
                || step
                    .parameters
                    .iter()
                    .any(|(key, value)| text_has_ref(key) || text_has_ref(value))
        }),
        ImplementationType::Compiled { provider_name } => text_has_ref(provider_name),
        ImplementationType::Primitive {
            provider_name,
            intent_description,
            command,
            cwd,
            env,
            suffix_args,
            operation,
            prompt,
            ..
        } => {
            provider_name.as_deref().is_some_and(text_has_ref)
                || intent_description.as_deref().is_some_and(text_has_ref)
                || command.as_deref().is_some_and(strings_have_ref)
                || cwd.as_deref().is_some_and(text_has_ref)
                || map_has_ref(env)
                || strings_have_ref(suffix_args)
                || operation.as_deref().is_some_and(text_has_ref)
                || prompt.as_ref().is_some_and(|prompt| {
                    text_has_ref(&prompt.name) || text_has_ref(&prompt.version)
                })
        },
        ImplementationType::Command {
            program,
            fixed_args,
            arg_mappings,
            suffix_args,
            env,
            ..
        } => {
            text_has_ref(program)
                || strings_have_ref(fixed_args)
                || arg_mappings.iter().any(mapping_has_ref)
                || strings_have_ref(suffix_args)
                || map_has_ref(env)
        },
    }
}

/// Check whether an action still contains unresolved inline refs.
pub fn has_unresolved_refs(action: &ExecutableAction) -> bool {
    let text_has_ref = |value: &str| value.contains(REDACTED_PREFIX) || value.contains(REF_PREFIX);
    let path_has_ref = |value: &std::path::Path| text_has_ref(&value.to_string_lossy());
    let json_has_ref = |root: &Value| {
        let mut pending = vec![root];
        while let Some(value) = pending.pop() {
            match value {
                Value::String(text) if text_has_ref(text) => return true,
                Value::Array(values) => pending.extend(values.iter()),
                Value::Object(values) => {
                    for (key, value) in values {
                        if text_has_ref(key) {
                            return true;
                        }
                        pending.push(value);
                    }
                },
                _ => {},
            }
        }
        false
    };

    match action {
        ExecutableAction::File(action) => match action {
            FileAction::Read { path, encoding } => {
                path_has_ref(path) || encoding.as_deref().is_some_and(text_has_ref)
            },
            FileAction::Write { path, content, .. } | FileAction::Append { path, content } => {
                path_has_ref(path) || text_has_ref(content)
            },
            FileAction::Delete { path, .. }
            | FileAction::Exists { path }
            | FileAction::CreateDir { path } => path_has_ref(path),
            FileAction::Copy {
                source,
                destination,
            }
            | FileAction::Move {
                source,
                destination,
            } => path_has_ref(source) || path_has_ref(destination),
            FileAction::List { path, pattern } => {
                path_has_ref(path) || pattern.as_deref().is_some_and(text_has_ref)
            },
        },
        ExecutableAction::Http(http) => {
            text_has_ref(&http.url)
                || http
                    .headers
                    .iter()
                    .any(|(key, value)| text_has_ref(key) || text_has_ref(value))
                || http.body.as_deref().is_some_and(text_has_ref)
                || http.content_type.as_deref().is_some_and(text_has_ref)
        },
        ExecutableAction::Bash(bash) => {
            text_has_ref(&bash.command)
                || bash.working_dir.as_deref().is_some_and(path_has_ref)
                || bash
                    .env
                    .iter()
                    .any(|(key, value)| text_has_ref(key) || text_has_ref(value))
                || bash.stdin.as_deref().is_some_and(text_has_ref)
        },
        ExecutableAction::DuckDb(duckdb) => {
            text_has_ref(&duckdb.sql)
                || duckdb.database.as_deref().is_some_and(text_has_ref)
                || text_has_ref(&duckdb.output_format)
        },
        ExecutableAction::Pack {
            capability_name,
            implementation,
            resolved_params,
        } => {
            text_has_ref(capability_name)
                || implementation_has_unresolved_refs(implementation, text_has_ref)
                || resolved_params
                    .iter()
                    .any(|(key, value)| text_has_ref(key) || json_has_ref(value))
        },
        ExecutableAction::SpawnSubGoal { goal, .. } => text_has_ref(goal),
        ExecutableAction::DelegateToAgent { targets } => targets.iter().any(|target| {
            text_has_ref(&target.target_agent_id)
                || text_has_ref(&target.context)
                || target
                    .input_artifact_ids
                    .iter()
                    .any(|value| text_has_ref(value))
                || target.input_data.as_ref().is_some_and(json_has_ref)
                || target.depth.as_deref().is_some_and(text_has_ref)
                || target
                    .spend_token_ids
                    .iter()
                    .any(|value| text_has_ref(value))
                || target
                    .required_capability
                    .as_deref()
                    .is_some_and(text_has_ref)
                || target.expected_artifacts.iter().any(|artifact| {
                    text_has_ref(&artifact.name)
                        || artifact.content_type.as_deref().is_some_and(text_has_ref)
                })
        }),
        ExecutableAction::HandoverToAgent {
            target_agent_id,
            context,
        } => text_has_ref(target_agent_id) || text_has_ref(context),
        ExecutableAction::SleepUntil { reason, .. } => reason.as_deref().is_some_and(text_has_ref),
    }
}

/// Sanitize action results before they re-enter LLM-visible observation flow.
pub fn sanitize_result(result: ActionResult, known_values: &KnownSecretValues) -> ActionResult {
    let replacements = known_value_replacements(known_values);
    if replacements.is_empty() {
        return result;
    }

    match result {
        ActionResult::Success | ActionResult::Bool { .. } | ActionResult::Binary { .. } => result,
        ActionResult::Text { content } => ActionResult::Text {
            content: replace_known_values(&content, &replacements),
        },
        ActionResult::Http {
            status,
            headers,
            body,
        } => ActionResult::Http {
            status,
            headers: headers
                .into_iter()
                .map(|(key, value)| (key, replace_known_values(&value, &replacements)))
                .collect(),
            body: replace_known_values(&body, &replacements),
        },
        ActionResult::List { items } => ActionResult::List {
            items: items
                .into_iter()
                .map(|item| replace_known_values(&item, &replacements))
                .collect(),
        },
        ActionResult::Browser { data } => ActionResult::Browser {
            data: sanitize_json_value(data, &replacements),
        },
    }
}
fn inject_header(
    action: ExecutableAction,
    fields: &HashMap<String, String>,
    name: &str,
    prefix: &Option<String>,
) -> Result<ExecutableAction, InjectionError> {
    let value = primary_field_value(fields)
        .ok_or_else(|| InjectionError::MissingField("value".to_string()))?;
    let header_value = match prefix {
        Some(prefix) => format!("{prefix}{value}"),
        None => value.to_string(),
    };

    match action {
        ExecutableAction::Http(mut http) => {
            http.headers.insert(name.to_string(), header_value);
            // A provisioned credential is delivered to its bound origin only,
            // like a user-typed one (P4): no redirects with it.
            http.carries_credential = true;
            Ok(ExecutableAction::Http(http))
        },
        _ => Err(InjectionError::UnsupportedTarget),
    }
}

fn inject_form_fields(
    action: ExecutableAction,
    fields: &HashMap<String, String>,
    mapping: &HashMap<String, String>,
) -> Result<ExecutableAction, InjectionError> {
    match action {
        ExecutableAction::Http(mut http) => {
            let mut body = if let Some(existing) = http.body.as_deref() {
                parse_json_object(existing)?
            } else {
                Map::new()
            };

            for (source_key, target_key) in mapping {
                let value = fields
                    .get(source_key)
                    .or_else(|| fields.get(&format!("value:{source_key}")))
                    .ok_or_else(|| InjectionError::MissingField(source_key.clone()))?;
                body.insert(target_key.clone(), Value::String(value.clone()));
            }

            http.body = Some(Value::Object(body).to_string());
            ensure_json_content_type(&mut http);
            http.carries_credential = true;
            Ok(ExecutableAction::Http(http))
        },
        _ => Err(InjectionError::UnsupportedTarget),
    }
}

fn inject_cookies(
    action: ExecutableAction,
    fields: &HashMap<String, String>,
    specs: &[CookieSpec],
) -> Result<ExecutableAction, InjectionError> {
    match action {
        ExecutableAction::Http(mut http) => {
            let mut cookie_parts = Vec::new();
            for (idx, spec) in specs.iter().enumerate() {
                let key = format!("cookie:{}", spec.name);
                let value = cookie_field_value(fields, idx, &spec.name)
                    .ok_or_else(|| InjectionError::MissingField(key.clone()))?;
                cookie_parts.push(format!("{}={}", spec.name, value));
            }

            let new_cookie_value = cookie_parts.join("; ");
            http.headers
                .entry("Cookie".to_string())
                .and_modify(|existing| {
                    if existing.is_empty() {
                        *existing = new_cookie_value.clone();
                    } else if !new_cookie_value.is_empty() {
                        *existing = format!("{existing}; {new_cookie_value}");
                    }
                })
                .or_insert(new_cookie_value);
            http.carries_credential = true;
            Ok(ExecutableAction::Http(http))
        },
        _ => Err(InjectionError::UnsupportedTarget),
    }
}

fn ensure_json_content_type(http: &mut HttpAction) {
    let has_content_type_header = http
        .headers
        .keys()
        .any(|key| key.eq_ignore_ascii_case("content-type"));
    if !has_content_type_header {
        http.headers
            .insert("Content-Type".to_string(), "application/json".to_string());
    }
    if http.content_type.is_none() {
        http.content_type = Some("application/json".to_string());
    }
}

fn parse_json_object(body: &str) -> Result<Map<String, Value>, InjectionError> {
    match serde_json::from_str::<Value>(body) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Err(InjectionError::NonJsonBody),
    }
}

fn primary_field_value(fields: &HashMap<String, String>) -> Option<&str> {
    if let Some(value) = fields.get("value") {
        return Some(value);
    }
    if fields.len() == 1 {
        return fields.values().next().map(String::as_str);
    }
    None
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::{ActionResult, HttpMethod};
    use crate::magician_v2::json_traversal::discard_json_iteratively;
    use base64::Engine;

    #[test]
    fn injects_header_into_http_action() {
        let mut fields = HashMap::new();
        fields.insert("value".to_string(), "token".to_string());

        let injected = inject(
            ExecutableAction::Http(HttpAction::get("https://api.example.com")),
            &fields,
            &InjectionTarget::Header {
                name: "Authorization".to_string(),
                prefix: Some("Bearer ".to_string()),
            },
        )
        .unwrap();

        let ExecutableAction::Http(http) = injected else {
            panic!("expected http action");
        };
        assert_eq!(
            http.headers.get("Authorization"),
            Some(&"Bearer token".to_string())
        );
        assert!(
            http.carries_credential,
            "a provisioned credential never follows a redirect either"
        );
    }

    #[test]
    fn merges_form_fields_into_json_body() {
        let mut fields = HashMap::new();
        fields.insert("card_number".to_string(), "4242".to_string());
        let mut mapping = HashMap::new();
        mapping.insert("card_number".to_string(), "number".to_string());

        let injected = inject(
            ExecutableAction::Http(HttpAction {
                method: HttpMethod::Post,
                url: "https://api.example.com/pay".to_string(),
                headers: HashMap::new(),
                body: Some("{\"existing\":true}".to_string()),
                content_type: Some("application/json".to_string()),
                timeout_secs: None,
                follow_redirects: true,
                carries_credential: false,
            }),
            &fields,
            &InjectionTarget::FormFields(mapping),
        )
        .unwrap();

        let ExecutableAction::Http(http) = injected else {
            panic!("expected http action");
        };
        let body: Value = serde_json::from_str(http.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["number"], "4242");
        assert_eq!(body["existing"], true);
    }

    #[test]
    fn constructs_cookie_header() {
        let mut fields = HashMap::new();
        fields.insert("cookie:0:session".to_string(), "abc123".to_string());

        let injected = inject(
            ExecutableAction::Http(HttpAction::get("https://api.example.com")),
            &fields,
            &InjectionTarget::Cookies(vec![CookieSpec {
                name: "session".to_string(),
                domain: "api.example.com".to_string(),
                path: "/".to_string(),
                secure: true,
                http_only: true,
                same_site: Some(SameSite::Lax),
                expires: None,
            }]),
        )
        .unwrap();

        let ExecutableAction::Http(http) = injected else {
            panic!("expected http action");
        };
        assert_eq!(
            http.headers.get("Cookie"),
            Some(&"session=abc123".to_string())
        );
    }

    #[test]
    fn constructs_cookie_header_with_indexed_duplicate_names() {
        let fields = HashMap::from([
            ("cookie:0:session".to_string(), "root-cookie".to_string()),
            ("cookie:1:session".to_string(), "admin-cookie".to_string()),
        ]);

        let injected = inject(
            ExecutableAction::Http(HttpAction::get("https://api.example.com/admin")),
            &fields,
            &InjectionTarget::Cookies(vec![
                CookieSpec {
                    name: "session".to_string(),
                    domain: "api.example.com".to_string(),
                    path: "/".to_string(),
                    secure: true,
                    http_only: true,
                    same_site: Some(SameSite::Lax),
                    expires: None,
                },
                CookieSpec {
                    name: "session".to_string(),
                    domain: "api.example.com".to_string(),
                    path: "/admin".to_string(),
                    secure: true,
                    http_only: true,
                    same_site: Some(SameSite::Lax),
                    expires: None,
                },
            ]),
        )
        .unwrap();

        let ExecutableAction::Http(http) = injected else {
            panic!("expected http action");
        };
        assert_eq!(
            http.headers.get("Cookie"),
            Some(&"session=root-cookie; session=admin-cookie".to_string())
        );
    }

    #[test]
    fn unresolved_ref_guard_detects_remaining_placeholders() {
        let action = ExecutableAction::Http(HttpAction::get(
            "https://api.example.com/[REDACTED:input-1]",
        ));

        assert!(has_unresolved_refs(&action));
    }

    #[test]
    fn unresolved_ref_guard_walks_recursive_parameters_without_serializing_the_action() {
        std::thread::Builder::new()
            .name("unresolved-ref-small-stack".to_string())
            .stack_size(256 * 1024)
            .spawn(|| {
                let mut nested = Value::String("[REF:credential]".to_string());
                for _ in 0..2_048 {
                    nested = Value::Array(vec![nested]);
                }
                let action = ExecutableAction::Pack {
                    capability_name: "test".to_string(),
                    implementation:
                        crate::magician_v2::execution::capability::ImplementationType::Compiled {
                            provider_name: "test".to_string(),
                        },
                    resolved_params: HashMap::from([("input".to_string(), nested)]),
                };

                assert!(has_unresolved_refs(&action));

                let ExecutableAction::Pack {
                    mut resolved_params,
                    ..
                } = action
                else {
                    unreachable!();
                };
                discard_json_iteratively(
                    resolved_params
                        .remove("input")
                        .expect("recursive parameter"),
                );
            })
            .expect("spawn small-stack unresolved-ref guard")
            .join()
            .expect("unresolved-ref guard remains stack safe");
    }

    #[test]
    fn unresolved_ref_guard_preserves_implementation_metadata_coverage() {
        let action = ExecutableAction::Pack {
            capability_name: "test".to_string(),
            implementation:
                crate::magician_v2::execution::capability::ImplementationType::Command {
                    program: "tool".to_string(),
                    fixed_args: vec!["[REF:credential]".to_string()],
                    arg_mappings: Vec::new(),
                    suffix_args: Vec::new(),
                    content_type: Default::default(),
                    env: HashMap::new(),
                },
            resolved_params: HashMap::new(),
        };

        assert!(has_unresolved_refs(&action));
    }

    #[test]
    fn sanitize_covers_text_http_browser_and_list() {
        let mut known_values = KnownSecretValues::new();
        known_values.insert("token".to_string(), "abcdefgh12345678".to_string());

        let text = sanitize_result(
            ActionResult::Text {
                content: "value abcdefgh12345678".to_string(),
            },
            &known_values,
        );
        let ActionResult::Text { content } = text else {
            panic!("expected text result");
        };
        assert_eq!(content, "value [REDACTED]");

        let http = sanitize_result(
            ActionResult::Http {
                status: 200,
                headers: HashMap::from([(
                    "Authorization".to_string(),
                    "Bearer abcdefgh12345678".to_string(),
                )]),
                body: "abcdefgh12345678".to_string(),
            },
            &known_values,
        );
        let ActionResult::Http { headers, body, .. } = http else {
            panic!("expected http");
        };
        assert_eq!(headers["Authorization"], "Bearer [REDACTED]");
        assert_eq!(body, "[REDACTED]");

        let browser = sanitize_result(
            ActionResult::Browser {
                data: serde_json::json!({"token":"abcdefgh12345678"}),
            },
            &known_values,
        );
        let ActionResult::Browser { data, .. } = browser else {
            panic!("expected browser");
        };
        assert_eq!(data["token"], "[REDACTED]");

        let list = sanitize_result(
            ActionResult::List {
                items: vec!["abcdefgh12345678".to_string()],
            },
            &known_values,
        );
        let ActionResult::List { items } = list else {
            panic!("expected list result");
        };
        assert_eq!(items, vec!["[REDACTED]".to_string()]);
    }

    #[test]
    fn sanitize_leaves_binary_success_and_bool_unchanged() {
        let mut known_values = KnownSecretValues::new();
        known_values.insert("token".to_string(), "abcdefgh12345678".to_string());

        assert!(matches!(
            sanitize_result(ActionResult::Success, &known_values),
            ActionResult::Success
        ));
        assert!(matches!(
            sanitize_result(ActionResult::Bool { value: true }, &known_values),
            ActionResult::Bool { value: true }
        ));
        assert!(matches!(
            sanitize_result(
                ActionResult::Binary {
                    data: "YWJj".to_string(),
                    mime_type: None
                },
                &known_values
            ),
            ActionResult::Binary { .. }
        ));
    }

    #[test]
    fn sanitize_redacts_short_secret_values() {
        let known_values =
            KnownSecretValues::from(HashMap::from([("otp".to_string(), "123456".to_string())]));

        let result = sanitize_result(
            ActionResult::Text {
                content: "OTP was 123456".to_string(),
            },
            &known_values,
        );

        let ActionResult::Text { content } = result else {
            panic!("expected text result");
        };
        assert_eq!(content, "OTP was [REDACTED]");
    }

    #[test]
    fn canonical_exact_redaction_preserves_ambiguous_domain_fields() {
        let result = sanitize_result(
            ActionResult::Browser {
                data: serde_json::json!({
                    "token": "pagination-cursor-27",
                    "secret": "surprise party",
                    "cookie": "chocolate chip",
                    "authorization": "domain approval granted"
                }),
            },
            &KnownSecretValues::new(),
        );
        let ActionResult::Browser { data } = result else {
            panic!("expected browser result");
        };
        assert_eq!(data["token"], "pagination-cursor-27");
        assert_eq!(data["secret"], "surprise party");
        assert_eq!(data["cookie"], "chocolate chip");
        assert_eq!(data["authorization"], "domain approval granted");
    }

    // ---- §15 item 4: encoded variants of known values are redacted ----------

    const VARIANT_SENTINEL: &str = "PLACEHOLDER-SENTINEL-VALUE-0123456789";

    fn sentinel_values(value: &str) -> KnownSecretValues {
        KnownSecretValues::from(HashMap::from([("k".to_string(), value.to_string())]))
    }

    fn redacted_text(input: &str, known: &KnownSecretValues) -> String {
        let ActionResult::Text { content } = sanitize_result(
            ActionResult::Text {
                content: input.to_string(),
            },
            known,
        ) else {
            panic!("expected text result");
        };
        content
    }

    #[test]
    fn sanitize_result_redacts_standard_base64_of_a_known_value() {
        let encoded = base64::engine::general_purpose::STANDARD.encode(VARIANT_SENTINEL);
        let out = redacted_text(
            &format!("blob {encoded} end"),
            &sentinel_values(VARIANT_SENTINEL),
        );
        assert!(
            !out.contains(&encoded),
            "base64 of the value survived: {out}"
        );
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn sanitize_result_redacts_url_safe_and_unpadded_base64_of_a_known_value() {
        use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
        let known = sentinel_values(VARIANT_SENTINEL);
        for encoded in [
            STANDARD_NO_PAD.encode(VARIANT_SENTINEL),
            URL_SAFE.encode(VARIANT_SENTINEL),
            URL_SAFE_NO_PAD.encode(VARIANT_SENTINEL),
        ] {
            let out = redacted_text(&format!("x {encoded} y"), &known);
            assert!(!out.contains(&encoded), "variant survived: {encoded}");
        }
    }

    #[test]
    fn sanitize_result_redacts_hex_of_a_known_value_in_both_cases() {
        let known = sentinel_values(VARIANT_SENTINEL);
        for encoded in [
            hex::encode(VARIANT_SENTINEL),
            hex::encode_upper(VARIANT_SENTINEL),
        ] {
            let out = redacted_text(&format!("dump: {encoded}"), &known);
            assert!(!out.contains(&encoded), "hex survived: {encoded}");
        }
    }

    #[test]
    fn sanitize_result_redacts_percent_encoded_known_value() {
        let value = "p@ss w0rd+/=?&#";
        let encoded = urlencoding::encode(value).into_owned();
        assert_ne!(encoded, value, "test value must actually percent-encode");
        let out = redacted_text(&format!("q={encoded}"), &sentinel_values(value));
        assert!(
            !out.contains(&encoded),
            "percent-encoded value survived: {out}"
        );
    }

    #[test]
    fn sanitize_result_redacts_json_escaped_known_value() {
        let value = r#"quote"back\slash-secret"#;
        let json = serde_json::to_string(value).expect("json string");
        let escaped = &json[1..json.len() - 1];
        assert_ne!(escaped, value, "test value must actually escape");
        let out = redacted_text(&format!("{{\"v\":\"{escaped}\"}}"), &sentinel_values(value));
        assert!(!out.contains(escaped), "json-escaped value survived: {out}");
    }

    #[test]
    fn short_known_values_keep_exact_match_but_gain_no_encoded_variants() {
        // A six-character OTP: its hex form is short enough to collide with
        // ordinary text, so only the exact value may be redacted.
        let known = sentinel_values("123456");
        assert_eq!(
            redacted_text("OTP was 123456", &known),
            "OTP was [REDACTED]"
        );
        let hex_form = hex::encode("123456");
        let out = redacted_text(&format!("ref {hex_form} ok"), &known);
        assert!(
            out.contains(&hex_form),
            "short value's hex was over-redacted: {out}"
        );
    }

    #[test]
    fn encoded_variants_are_redacted_inside_browser_json_results() {
        let encoded = base64::engine::general_purpose::STANDARD.encode(VARIANT_SENTINEL);
        let result = sanitize_result(
            ActionResult::Browser {
                data: serde_json::json!({ "stdout": format!("token {encoded}") }),
            },
            &sentinel_values(VARIANT_SENTINEL),
        );
        let ActionResult::Browser { data } = result else {
            panic!("expected browser result");
        };
        let rendered = data.to_string();
        assert!(
            !rendered.contains(&encoded),
            "base64 survived in browser json: {rendered}"
        );
    }

    // ---- §15 item 6: the redaction table is sealed and zeroizes --------------
}
