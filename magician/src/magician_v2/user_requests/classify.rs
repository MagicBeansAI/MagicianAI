//! Classification of a pending request's sensitivity, done once at acceptance.
//! Typed metadata decides; prompt wording is a conservative fallback that can
//! only raise sensitivity. Everything downstream reads the stored spec.
use super::*;
use crate::magician_v2::secrets::classify::{
    classify_form_fields, kind_from_prompt_wording, FormFieldSpec,
};

/// One-time collection never waits longer than this (spec §3.3 / the built
/// browser JIT path).
pub(crate) const ONE_TIME_COLLECTION_MAX_SECS: u64 = 180;

/// Classify a request's sensitivity. Returns the producer's own spec untouched
/// when it set one; otherwise derives one from typed metadata first and
/// prompt wording last. `now_ms` is the base for the collection deadline when
/// the service has not yet stamped `created_at`.
pub(crate) fn classify_sensitive(request: &UserRequest, now_ms: i64) -> Option<SensitiveInputSpec> {
    if let Some(spec) = &request.sensitive {
        return Some(spec.clone());
    }
    let base_ms = if request.created_at > 0 {
        request.created_at
    } else {
        now_ms
    };
    let deadline = |one_time: bool| -> i64 {
        let secs = if one_time {
            request.timeout_secs.clamp(1, ONE_TIME_COLLECTION_MAX_SECS)
        } else {
            request.timeout_secs
        };
        base_ms.saturating_add((secs as i64).saturating_mul(1000))
    };
    let single =
        |kind: SensitiveKind, provenance: SensitiveProvenance, one_time: bool| SensitiveInputSpec {
            kind: Some(kind),
            fields: Vec::new(),
            provenance,
            one_time,
            collection_deadline_ms: deadline(one_time),
            challenge_id: None,
            revision: 0,
            expected_destination: None,
        };
    let input_type = request
        .context
        .get("input_type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let input_type = input_type.as_str();
    let one_time_lifetime = request
        .context
        .get("credential_lifetime")
        .and_then(serde_json::Value::as_str)
        == Some("one_time");

    // 1. A trusted in-process producer: the browser one-time credential path.
    if request.request_type == SECURE_INPUT_REQUEST {
        return Some(single(
            SensitiveKind::Password,
            SensitiveProvenance::Producer,
            true,
        ));
    }
    // A request that collects no free-text value has nothing to protect: a
    // choice, confirmation, approval, or external-action prompt resolves by
    // decision, and its wording ("approve the token refresh?") must never turn
    // that decision into a cancel or shorten its window. Most decision
    // producers send no `input_type` at all and carry their options instead,
    // exactly as the event layer renders them. The one-time fill confirmation
    // is one such prompt.
    // ...unless one of those options asks for a value. `requires_input` comes
    // verbatim from a producer's or a model's tool argument, and an option that
    // sets it collects free text exactly as a typed ask does. Inferring
    // "decision" from the mere presence of options made this branch an implicit
    // ALLOW that skipped the wording fallback below — the one pass written to
    // catch an absent or off-schema `input_type`. An ask worded "paste your API
    // key", carrying one `requires_input` option and no `input_type`, was
    // therefore never classified: its answer took the ordinary path into the
    // history shard and the lifecycle events instead of `Zeroizing` custody,
    // and the UI, which reads masking off this spec, rendered it in the clear.
    // An approval prompt still carries `requires_input: false` options, so the
    // day-long windows the comment above protects are untouched.
    let decision_prompt = input_type.is_empty()
        && !request.options.is_empty()
        && !request.options.iter().any(|option| option.requires_input);
    // A DENY list of the decision types, not an allow list of the free-text
    // ones: the decision types are finite and known (`UserInputType::type_name`),
    // while an `input_type` comes verbatim from a model's tool argument. Naming
    // the free-text ones meant every unknown or off-schema spelling — `secret`,
    // `code`, `textarea` — fell through as "no free-text value to protect",
    // which is the one direction this gate must never fail in.
    if request.request_type == SECURE_CONFIRM_REQUEST
        || decision_prompt
        || matches!(
            input_type,
            "choice"
                | "multi_choice"
                | "confirmation"
                | "external_action"
                | "file_path"
                | "tool_authorization"
                | "sandbox_override"
                | "diff_approval"
        )
    {
        return None;
    }
    // 2. Typed input.
    if input_type == "password" {
        return Some(single(
            SensitiveKind::Password,
            SensitiveProvenance::TypedInput,
            one_time_lifetime,
        ));
    }
    if input_type == "otp" {
        return Some(single(
            SensitiveKind::Otp,
            SensitiveProvenance::TypedInput,
            true,
        ));
    }
    // 3. Form schema. Read whenever the request CARRIES questions, not only
    //    when its `input_type` says `form`: a single-question call keeps
    //    `input_type: "text"` (the chat dispatcher promotes to `form` only at
    //    two or more), and skipping the schema there discarded the typed kind —
    //    the most trusted signal in the design — for exactly the shape a model
    //    uses to ask for one password.
    if let Some(spec) = classify_form(request, &deadline) {
        return Some(spec);
    }
    // 4. Wording only — conservative, and never a downgrade of the above. A
    //    prompt that reads as a one-time code is one-time material.
    if let Some(kind) = kind_from_prompt_wording(&request.question) {
        let one_time = kind == SensitiveKind::Otp;
        return Some(single(kind, SensitiveProvenance::Heuristic, one_time));
    }
    None
}

fn classify_form(
    request: &UserRequest,
    deadline: &dyn Fn(bool) -> i64,
) -> Option<SensitiveInputSpec> {
    let questions = request
        .context
        .get("input_schema")
        .and_then(|schema| schema.get("questions"))
        .and_then(serde_json::Value::as_array)?;
    fn field<'a>(q: &'a serde_json::Value, key: &str) -> &'a str {
        q.get(key).and_then(serde_json::Value::as_str).unwrap_or("")
    }
    let specs: Vec<FormFieldSpec<'_>> = questions
        .iter()
        .map(|q| FormFieldSpec {
            id: field(q, "id"),
            prompt: field(q, "prompt"),
            input_type: field(q, "input_type"),
            flagged: q.get("sensitive").and_then(serde_json::Value::as_bool) == Some(true),
            has_options: q
                .get("options")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|options| !options.is_empty()),
        })
        .collect();
    let (fields, typed) = classify_form_fields(&specs);
    if fields.is_empty() {
        return None;
    }
    let one_time = fields.iter().any(|(_, kind)| *kind == SensitiveKind::Otp);
    Some(SensitiveInputSpec {
        kind: None,
        fields: fields
            .into_iter()
            .map(|(id, kind)| SensitiveField { id, kind })
            .collect(),
        provenance: if typed {
            SensitiveProvenance::FormSchema
        } else {
            SensitiveProvenance::Heuristic
        },
        one_time,
        collection_deadline_ms: deadline(one_time),
        challenge_id: None,
        revision: 0,
        expected_destination: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_700_000_000_000;

    fn request(request_type: &str, question: &str, context: serde_json::Value) -> UserRequest {
        UserRequest {
            id: String::new(),
            request_type: request_type.into(),
            question: question.into(),
            options: vec![],
            principal: "owner".into(),
            workspace: "workspace".into(),
            context,
            source: "chat".into(),
            execution_id: Some("exec".into()),
            task_id: None,
            timeout_secs: 600,
            default_on_timeout: "timeout".into(),
            created_at: 0,
            sensitive: None,
        }
    }

    #[test]
    fn plain_question_is_ordinary() {
        let r = request(
            "need_user_input",
            "What city should the meeting be in?",
            json!({ "input_type": "text" }),
        );
        assert_eq!(classify_sensitive(&r, NOW), None);
    }

    #[test]
    fn typed_password_input_is_password() {
        let r = request(
            "need_user_input",
            "Sign in",
            json!({ "input_type": "password" }),
        );
        let spec = classify_sensitive(&r, NOW).expect("typed password is sensitive");
        assert_eq!(spec.kind, Some(SensitiveKind::Password));
        assert_eq!(spec.provenance, SensitiveProvenance::TypedInput);
        assert!(!spec.one_time);
        assert_eq!(spec.collection_deadline_ms, NOW + 600_000);
    }

    #[test]
    fn deadline_uses_created_at_once_the_service_stamped_it() {
        let mut r = request(
            "need_user_input",
            "Sign in",
            json!({ "input_type": "password" }),
        );
        r.created_at = NOW - 5_000;
        let spec = classify_sensitive(&r, NOW).unwrap();
        assert_eq!(spec.collection_deadline_ms, NOW - 5_000 + 600_000);
    }

    #[test]
    fn secure_browser_input_is_a_one_time_password_from_the_producer() {
        let mut r = request(
            "secure_browser_input",
            "Private input",
            json!({ "input_type": "password", "credential_lifetime": "one_time" }),
        );
        r.timeout_secs = 3600;
        let spec = classify_sensitive(&r, NOW).unwrap();
        assert_eq!(spec.kind, Some(SensitiveKind::Password));
        assert_eq!(spec.provenance, SensitiveProvenance::Producer);
        assert!(spec.one_time);
        assert!(
            spec.collection_deadline_ms <= NOW + 180_000,
            "one-time collection is clamped to 180 s"
        );
    }

    #[test]
    fn form_with_a_password_question_flags_it_and_the_identifier_beside_it() {
        let r = request(
            "need_user_input",
            "Log in to the portal",
            json!({
                "input_type": "form",
                "input_schema": { "questions": [
                    { "id": "email", "prompt": "Email address", "input_type": "text" },
                    { "id": "pw", "prompt": "Password", "input_type": "password" },
                    { "id": "remember", "prompt": "Remember me?", "input_type": "choice" }
                ]}
            }),
        );
        let spec = classify_sensitive(&r, NOW).unwrap();
        assert_eq!(spec.kind, None);
        assert_eq!(spec.provenance, SensitiveProvenance::FormSchema);
        let mut fields = spec.fields.clone();
        fields.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(
            fields,
            vec![
                SensitiveField {
                    id: "email".into(),
                    kind: SensitiveKind::LoginIdentifier
                },
                SensitiveField {
                    id: "pw".into(),
                    kind: SensitiveKind::Password
                },
            ]
        );
    }

    #[test]
    fn form_with_an_otp_typed_question_is_otp_and_one_time() {
        let r = request(
            "need_user_input",
            "Verify",
            json!({
                "input_type": "form",
                "input_schema": { "questions": [
                    { "id": "code", "prompt": "Code", "input_type": "otp" }
                ]}
            }),
        );
        let spec = classify_sensitive(&r, NOW).unwrap();
        assert_eq!(
            spec.fields,
            vec![SensitiveField {
                id: "code".into(),
                kind: SensitiveKind::Otp
            }]
        );
        assert!(spec.one_time);
    }

    #[test]
    fn an_email_in_a_non_authentication_form_stays_ordinary() {
        let r = request(
            "need_user_input",
            "Who should get the report?",
            json!({
                "input_type": "form",
                "input_schema": { "questions": [
                    { "id": "email", "prompt": "Recipient email", "input_type": "text" },
                    { "id": "cc", "prompt": "Anyone to cc?", "input_type": "text" }
                ]}
            }),
        );
        assert_eq!(classify_sensitive(&r, NOW), None);
    }

    #[test]
    fn otp_worded_question_is_a_one_time_code_by_heuristic() {
        let r = request(
            "need_user_input",
            "Enter the verification code we sent to your phone",
            json!({ "input_type": "text" }),
        );
        let spec = classify_sensitive(&r, NOW).unwrap();
        assert_eq!(spec.kind, Some(SensitiveKind::Otp));
        assert_eq!(spec.provenance, SensitiveProvenance::Heuristic);
        assert!(spec.one_time);
        assert!(spec.collection_deadline_ms <= NOW + 180_000);
    }

    #[test]
    fn secret_worded_question_without_code_wording_is_other() {
        let r = request(
            "need_user_input",
            "What is your account PIN?",
            json!({ "input_type": "text" }),
        );
        let spec = classify_sensitive(&r, NOW).unwrap();
        assert_eq!(spec.kind, Some(SensitiveKind::Other));
        assert!(!spec.one_time);
    }

    #[test]
    fn form_question_worded_like_a_secret_is_flagged_by_heuristic() {
        let r = request(
            "need_user_input",
            "Account details",
            json!({
                "input_type": "form",
                "input_schema": { "questions": [
                    { "id": "q1", "prompt": "Your account PIN", "input_type": "text" },
                    { "id": "q2", "prompt": "Preferred branch", "input_type": "text" }
                ]}
            }),
        );
        let spec = classify_sensitive(&r, NOW).unwrap();
        assert_eq!(spec.provenance, SensitiveProvenance::Heuristic);
        assert_eq!(
            spec.fields,
            vec![SensitiveField {
                id: "q1".into(),
                kind: SensitiveKind::Other
            }]
        );
    }

    #[test]
    fn heuristic_never_downgrades_a_typed_password() {
        let r = request(
            "need_user_input",
            "What is your favourite colour?",
            json!({ "input_type": "password" }),
        );
        let spec = classify_sensitive(&r, NOW).unwrap();
        assert_eq!(spec.kind, Some(SensitiveKind::Password));
        assert_eq!(spec.provenance, SensitiveProvenance::TypedInput);
    }

    #[test]
    fn an_explicit_producer_spec_is_preserved_untouched() {
        let mut r = request(
            "need_user_input",
            "What city?",
            json!({ "input_type": "text" }),
        );
        let producer = SensitiveInputSpec {
            kind: Some(SensitiveKind::Otp),
            fields: vec![],
            provenance: SensitiveProvenance::Producer,
            one_time: true,
            collection_deadline_ms: NOW + 1,
            challenge_id: Some("ch-1".into()),
            revision: 2,
            expected_destination: None,
        };
        r.sensitive = Some(producer.clone());
        assert_eq!(classify_sensitive(&r, NOW), Some(producer));
    }

    #[test]
    fn a_decision_only_request_is_never_classified() {
        for input_type in [
            "choice",
            "multi_choice",
            "confirmation",
            "external_action",
            "Choice",
        ] {
            let r = request(
                "need_user_input",
                "Approve the token refresh for the secret vault?",
                json!({ "input_type": input_type }),
            );
            assert_eq!(classify_sensitive(&r, NOW), None, "{input_type}");
        }
        let r = request(
            "secure_browser_confirm",
            "Fill https://pin.example.com?",
            json!({ "input_type": "choice" }),
        );
        assert_eq!(classify_sensitive(&r, NOW), None);
        // The production shape of every approval/notification/relay producer:
        // no `input_type` at all, options set. OTP wording must not classify
        // it either — that would clamp a day-long prompt to three minutes.
        let mut r = request(
            "mcp_tool_approval",
            "The site asked for a two-factor code; approve calling create_token?",
            json!({}),
        );
        r.options = vec![
            RequestOption {
                id: "approve_once".into(),
                label: "Approve".into(),
                requires_input: false,
            },
            RequestOption {
                id: "deny".into(),
                label: "Deny".into(),
                requires_input: false,
            },
        ];
        assert_eq!(classify_sensitive(&r, NOW), None);
        // Without options, an untyped request is a free-text ask and is classified.
        let r = request(
            "need_user_input",
            "Enter the verification code we sent",
            json!({}),
        );
        assert!(classify_sensitive(&r, NOW).is_some());
    }

    /// An option that asks for a value makes the ask a collector, whatever it
    /// looks like otherwise. Before this, "has options" alone meant "decision",
    /// and a decision is never classified — so an ask worded for a secret, with
    /// one `requires_input` option and no `input_type`, slipped past the wording
    /// fallback and its answer was stored and published as ordinary text.
    #[test]
    fn an_option_that_collects_a_value_does_not_make_the_ask_a_decision() {
        let collecting = |label: &str| RequestOption {
            id: "provide".into(),
            label: label.into(),
            requires_input: true,
        };
        let deciding = |id: &str| RequestOption {
            id: id.into(),
            label: id.into(),
            requires_input: false,
        };

        // The shape that escaped: secret wording, options, no `input_type`.
        let mut r = request(
            "need_user_input",
            "Paste your API key to continue",
            json!({}),
        );
        r.options = vec![collecting("Provide the key"), deciding("cancel")];
        let spec = classify_sensitive(&r, NOW).expect("a collecting option is classified");
        assert_eq!(spec.kind, Some(SensitiveKind::Other));
        assert_eq!(spec.provenance, SensitiveProvenance::Heuristic);

        // A one-time code asked the same way keeps its one-time lifetime.
        let mut otp = request(
            "need_user_input",
            "Enter the verification code we sent",
            json!({}),
        );
        otp.options = vec![collecting("Enter the code")];
        let spec = classify_sensitive(&otp, NOW).expect("a collecting option is classified");
        assert_eq!(spec.kind, Some(SensitiveKind::Otp));
        assert!(spec.one_time);

        // And the case the exemption exists for is untouched: an approval whose
        // options decide and collect nothing stays unclassified, so its wording
        // cannot clamp its window.
        let mut approval = request(
            "mcp_tool_approval",
            "The site asked for a two-factor code; approve calling create_token?",
            json!({}),
        );
        approval.options = vec![deciding("approve_once"), deciding("deny")];
        assert_eq!(classify_sensitive(&approval, NOW), None);
    }

    #[test]
    fn a_guidance_request_is_classified_like_text() {
        let r = request(
            "need_user_input",
            "Paste the API token to use",
            json!({ "input_type": "guidance" }),
        );
        assert!(classify_sensitive(&r, NOW).is_some());
    }

    #[test]
    fn an_otp_worded_form_field_is_a_one_time_code_not_a_password() {
        let r = request(
            "need_user_input",
            "Verify",
            json!({
                "input_type": "form",
                "input_schema": { "questions": [
                    { "id": "code", "prompt": "One-time password from your authenticator", "input_type": "text" }
                ]}
            }),
        );
        let spec = classify_sensitive(&r, NOW).unwrap();
        assert_eq!(
            spec.fields,
            vec![SensitiveField {
                id: "code".into(),
                kind: SensitiveKind::Otp
            }]
        );
        assert!(spec.one_time);
    }

    #[test]
    fn a_totp_prompt_is_secret_prose() {
        let r = request(
            "need_user_input",
            "Enter your TOTP",
            json!({ "input_type": "text" }),
        );
        assert!(classify_sensitive(&r, NOW).is_some());
    }

    #[test]
    fn a_password_worded_form_field_is_a_password_and_binds_its_identifier() {
        let r = request(
            "need_user_input",
            "Log in",
            json!({
                "input_type": "form",
                "input_schema": { "questions": [
                    { "id": "email", "prompt": "Email", "input_type": "text" },
                    { "id": "pwd", "prompt": "Password", "input_type": "text" }
                ]}
            }),
        );
        let spec = classify_sensitive(&r, NOW).unwrap();
        let mut fields = spec.fields.clone();
        fields.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(
            fields,
            vec![
                SensitiveField {
                    id: "email".into(),
                    kind: SensitiveKind::LoginIdentifier
                },
                SensitiveField {
                    id: "pwd".into(),
                    kind: SensitiveKind::Password
                },
            ]
        );
        assert_eq!(spec.provenance, SensitiveProvenance::Heuristic);
    }

    #[test]
    fn prose_matches_secret_words_not_substrings() {
        let ordinary = request(
            "need_user_input",
            "Which tokens should the summary keep? Mind the carbon footprint.",
            json!({ "input_type": "text" }),
        );
        assert_eq!(classify_sensitive(&ordinary, NOW), None);
        let secret = request(
            "need_user_input",
            "Paste the API token here",
            json!({ "input_type": "text" }),
        );
        assert!(classify_sensitive(&secret, NOW).is_some());
    }
}
