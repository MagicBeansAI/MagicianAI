//! Recurring Monitors Phase 4 — shared core for the `preview_monitor` /
//! `create_monitor` / `update_monitor` compiled chat tools.
//!
//! Plan: `docs/plans/2026-07-21-recurring-monitors-productization-design-implementation.md`
//! §5.1 (create-from-chat journey), §8 (tool surface: "Add compiled
//! `preview_monitor`, `create_monitor`, and `update_monitor` tools for chat.
//! Their schemas and guides live in capability YAML").
//!
//! Design contract:
//!
//! * **Interpretation happens in the calling model.** The chat agent turns
//!   the user's natural-language ask into the tools' structured args, guided
//!   by the YAML `guide` (the plan's designated home for tool guidance). The
//!   tools themselves are DETERMINISTIC — no nested LLM call, so there is no
//!   LLM-backed compilation prompt to register; validation and normalization
//!   are exactly the Phase 1 admission gate (`validate_and_normalize`).
//! * **Preview never creates.** `preview_monitor` validates + normalizes,
//!   renders the interpreted contract (spec + exact schedule + the same
//!   `cadence_summary` string the list rows show) and returns a
//!   `preview_fingerprint` over the normalized contract.
//! * **Create requires the previewed contract.** `create_monitor` recomputes
//!   the fingerprint from its own (re-validated) args and refuses when the
//!   caller's `preview_fingerprint` is absent (`monitor_preview_required`)
//!   or does not match (`monitor_preview_stale`) — review-before-recurring-
//!   spend is enforced in data, not prompt convention (plan §4 rule 2).
//! * **One service path.** Create goes through the SAME
//!   `monitors_api::create_monitor_task` composition the HTTP route uses
//!   (canonical `create_task` + spec attach via `update_task`, server-owned
//!   revision). Update mirrors `PATCH /monitors/{task_id}` (load → merge →
//!   re-validate → `update_task`; a spec edit bumps the revision). Plain
//!   tasks are never reachable (`monitor_not_found`), so generic `create_task`
//!   behavior is untouched.
//!
//! The thin `preview_monitor.rs` / `create_monitor.rs` / `update_monitor.rs`
//! handlers extract scope + service and delegate here so tests can drive the
//! full logic against `build_test_artifact_v2_service` without constructing
//! `AgentResources`.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::{ArtifactV2Service, ScopeRef, UpdateTaskInput, V3ReadApi};
use crate::magician_v2::monitor_support::{
    cadence_summary, create_monitor_task, monitor_state_label, monitor_title_from, parsed_schedule,
    schedule_is_paused, DEFAULT_MONITOR_AGENT_ID,
};
use crate::magician_v2::monitors::monitor_spec::{
    monitor_contract_fingerprint, validate_and_normalize, MonitorMatchMode,
    MonitorNotificationPolicy, MonitorSources, MonitorSpecV1, MONITOR_SPEC_SCHEMA_VERSION,
};
use crate::magician_v2::storage::task_models::TaskSchedule;

/// Stable reasons for the preview→create binding (Phase 1 error style).
pub const REASON_PREVIEW_REQUIRED: &str = "monitor_preview_required";
pub const REASON_PREVIEW_STALE: &str = "monitor_preview_stale";
pub const REASON_MONITOR_NOT_FOUND: &str = "monitor_not_found";

fn error_response(reason: impl Into<String>, hint: Option<&str>) -> Value {
    let mut body = json!({
        "status": "error",
        "reason": reason.into(),
    });
    if let Some(hint) = hint {
        body["hint"] = json!(hint);
    }
    body
}

/// Read a string-list arg. Tolerates a bare string (models emit both
/// shapes); non-string entries are rejected with a stable reason.
fn string_list_arg(args: &Value, key: &str) -> Result<Option<Vec<String>>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(single)) => Ok(Some(vec![single.clone()])),
        Some(Value::Array(entries)) => {
            let mut list = Vec::with_capacity(entries.len());
            for entry in entries {
                match entry.as_str() {
                    Some(value) => list.push(value.to_string()),
                    None => return Err(format!("monitor_{key}_entry_not_a_string")),
                }
            }
            Ok(Some(list))
        },
        Some(_) => Err(format!("monitor_{key}_not_a_list")),
    }
}

fn optional_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn match_mode_arg(args: &Value) -> Result<Option<MonitorMatchMode>, String> {
    match optional_str(args, "match_mode") {
        None => Ok(None),
        Some(raw) => serde_json::from_value::<MonitorMatchMode>(Value::String(raw))
            .map(Some)
            .map_err(|_| "monitor_match_mode_invalid".to_string()),
    }
}

fn notification_policy_arg(args: &Value) -> Result<Option<MonitorNotificationPolicy>, String> {
    match optional_str(args, "notification_policy") {
        None => Ok(None),
        Some(raw) => serde_json::from_value::<MonitorNotificationPolicy>(Value::String(raw))
            .map(Some)
            .map_err(|_| "monitor_notification_policy_invalid".to_string()),
    }
}

/// Build a full `MonitorSpecV1` from the flat tool args (preview/create).
/// Missing optional fields take the product defaults (balanced matching,
/// material-changes-only notifications, quiet baseline). The returned spec
/// is ALREADY validated + normalized by the Phase 1 admission gate.
pub fn spec_from_args(args: &Value) -> Result<MonitorSpecV1, String> {
    let objective = optional_str(args, "objective").ok_or("monitor_objective_required")?;
    let mut spec = MonitorSpecV1 {
        schema_version: MONITOR_SPEC_SCHEMA_VERSION,
        objective,
        query_seeds: string_list_arg(args, "query_seeds")?.unwrap_or_default(),
        sources: MonitorSources {
            urls: string_list_arg(args, "urls")?.unwrap_or_default(),
            domains: string_list_arg(args, "domains")?.unwrap_or_default(),
            authenticated_sources: string_list_arg(args, "authenticated_sources")?
                .unwrap_or_default(),
        },
        include_rules: string_list_arg(args, "include_rules")?.unwrap_or_default(),
        exclude_rules: string_list_arg(args, "exclude_rules")?.unwrap_or_default(),
        match_mode: match_mode_arg(args)?.unwrap_or(MonitorMatchMode::Balanced),
        notification_policy: notification_policy_arg(args)?
            .unwrap_or(MonitorNotificationPolicy::MaterialChanges),
        notify_initial_baseline: args
            .get("notify_initial_baseline")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    };
    validate_and_normalize(&mut spec)?;
    Ok(spec)
}

/// Parse the optional `schedule` arg with the EXISTING task schedule parser
/// (`TaskSchedule` — the same shape `create_task` documents). Returns the
/// raw JSON to persist, exactly like the HTTP routes do.
pub fn schedule_from_args(args: &Value) -> Result<Option<Value>, String> {
    match args.get("schedule").filter(|value| !value.is_null()) {
        None => Ok(None),
        Some(value) => match serde_json::from_value::<TaskSchedule>(value.clone()) {
            Ok(_) => Ok(Some(value.clone())),
            Err(error) => Err(format!("monitor_schedule_invalid: {error}")),
        },
    }
}

/// Render the interpreted contract block shared by preview and create
/// responses — the chat review card renders from exactly this data.
fn contract_block(spec: &MonitorSpecV1, schedule: Option<&Value>, title: &str) -> Value {
    let parsed = parsed_schedule(schedule);
    json!({
        "title": title,
        "spec": spec,
        "schedule": schedule,
        "cadence_summary": cadence_summary(parsed.as_ref()),
    })
}

/// `preview_monitor` core: validate, normalize, and return the interpreted
/// contract + fingerprint WITHOUT touching the task store.
pub fn preview_monitor_response(args: &Value) -> Value {
    let spec = match spec_from_args(args) {
        Ok(spec) => spec,
        Err(reason) => return error_response(reason, Some(SPEC_HINT)),
    };
    let schedule = match schedule_from_args(args) {
        Ok(schedule) => schedule,
        Err(reason) => return error_response(reason, Some(SCHEDULE_HINT)),
    };
    let title = monitor_title_from(optional_str(args, "title"), &spec);
    let fingerprint = monitor_contract_fingerprint(&spec, schedule.as_ref());
    json!({
        "status": "previewed",
        "created": false,
        "contract": contract_block(&spec, schedule.as_ref(), &title),
        "preview_fingerprint": fingerprint,
        "next_step": "Show the user this contract (objective, sources, schedule + timezone, match mode, notification policy) as a compact review card. Only after the user explicitly confirms, call create_monitor with the SAME arguments plus this preview_fingerprint. If the user edits anything, call preview_monitor again first.",
    })
}

const SCHEDULE_HINT: &str = "schedule must be a TaskSchedule object, e.g. {\"kind\":{\"Cron\":{\"expression\":\"0 9 * * *\",\"timezone\":\"America/New_York\"}}}. Ask the user for the missing cadence rather than inventing one.";

const SPEC_HINT: &str = "Fix the flagged field and call preview_monitor again. A monitor needs an objective plus at least one of: urls, domains, or query_seeds.";

/// `create_monitor` core: re-validate, enforce the preview fingerprint, then
/// create through the ONE service path the HTTP route uses.
pub async fn create_monitor_response(
    service: &Arc<ArtifactV2Service>,
    scope: &ScopeRef,
    args: &Value,
) -> Value {
    let spec = match spec_from_args(args) {
        Ok(spec) => spec,
        Err(reason) => return error_response(reason, Some(SPEC_HINT)),
    };
    let schedule = match schedule_from_args(args) {
        Ok(schedule) => schedule,
        Err(reason) => return error_response(reason, Some(SCHEDULE_HINT)),
    };

    let expected = monitor_contract_fingerprint(&spec, schedule.as_ref());
    match optional_str(args, "preview_fingerprint") {
        None => {
            return error_response(
                REASON_PREVIEW_REQUIRED,
                Some("Call preview_monitor first, show the user the interpreted contract, and pass the returned preview_fingerprint here after they confirm. Never skip the review step."),
            );
        },
        Some(provided) if provided != expected => {
            return error_response(
                REASON_PREVIEW_STALE,
                Some("The arguments no longer match the previewed contract. Call preview_monitor again with the current arguments and re-confirm with the user before creating."),
            );
        },
        Some(_) => {},
    }

    let title = optional_str(args, "title");
    match create_monitor_task(
        service,
        scope,
        title,
        spec,
        schedule,
        DEFAULT_MONITOR_AGENT_ID.to_string(),
    )
    .await
    {
        Ok(task) => {
            let parsed = parsed_schedule(task.manifest.schedule.as_ref());
            json!({
                "status": "created",
                "task_id": task.manifest.task_id,
                "title": task.manifest.title,
                "monitor_revision": task.manifest.monitor_revision,
                "state": monitor_state_label(schedule_is_paused(parsed.as_ref())),
                "cadence_summary": cadence_summary(parsed.as_ref()),
                "manage_url": "/tasks?type=monitors",
            })
        },
        Err(error) => error_response(format!("create_monitor failed: {error}"), None),
    }
}

/// `update_monitor` core: mirrors `PATCH /monitors/{task_id}` — 404-style
/// refusal for plain tasks, field-level spec merge, Phase 1 re-validation,
/// server-owned revision bump on spec edits.
pub async fn update_monitor_response(
    service: &Arc<ArtifactV2Service>,
    scope: &ScopeRef,
    args: &Value,
) -> Value {
    let Some(task_id) = optional_str(args, "task_id") else {
        return error_response(
            "update_monitor requires a non-empty `task_id`",
            Some(
                "Find the monitor's task id via list_tasks or the monitors list before editing it.",
            ),
        );
    };

    let task = match V3ReadApi::get_task(service.as_ref(), scope, &task_id).await {
        Ok(task) => task,
        Err(error) => return error_response(format!("update_monitor failed: {error}"), None),
    };
    let Some(current_spec) = task.manifest.monitor_spec.clone() else {
        return error_response(
            REASON_MONITOR_NOT_FOUND,
            Some("That task is a plain task, not a monitor. update_monitor only edits monitors; use update_task for ordinary tasks."),
        );
    };

    // Field-level merge: any provided spec field replaces the current value
    // (an explicitly-passed empty list clears it); absent fields are kept.
    let mut spec = current_spec.clone();
    let mut spec_touched = false;
    if let Some(objective) = optional_str(args, "objective") {
        spec.objective = objective;
        spec_touched = true;
    }
    macro_rules! merge_list {
        ($key:literal, $target:expr) => {
            match string_list_arg(args, $key) {
                Ok(Some(list)) => {
                    $target = list;
                    spec_touched = true;
                },
                Ok(None) => {},
                Err(reason) => return error_response(reason, None),
            }
        };
    }
    merge_list!("urls", spec.sources.urls);
    merge_list!("domains", spec.sources.domains);
    merge_list!("authenticated_sources", spec.sources.authenticated_sources);
    merge_list!("query_seeds", spec.query_seeds);
    merge_list!("include_rules", spec.include_rules);
    merge_list!("exclude_rules", spec.exclude_rules);
    match match_mode_arg(args) {
        Ok(Some(mode)) => {
            spec.match_mode = mode;
            spec_touched = true;
        },
        Ok(None) => {},
        Err(reason) => return error_response(reason, None),
    }
    match notification_policy_arg(args) {
        Ok(Some(policy)) => {
            spec.notification_policy = policy;
            spec_touched = true;
        },
        Ok(None) => {},
        Err(reason) => return error_response(reason, None),
    }
    if let Some(notify_baseline) = args.get("notify_initial_baseline").and_then(Value::as_bool) {
        spec.notify_initial_baseline = notify_baseline;
        spec_touched = true;
    }
    if spec_touched {
        if let Err(reason) = validate_and_normalize(&mut spec) {
            return error_response(reason, Some(SPEC_HINT));
        }
    }

    let schedule = match schedule_from_args(args) {
        Ok(schedule) => schedule,
        Err(reason) => return error_response(reason, Some(SCHEDULE_HINT)),
    };
    let title = optional_str(args, "title");
    if !spec_touched && schedule.is_none() && title.is_none() {
        return error_response(
            "update_monitor requires at least one field to update",
            Some("Pass any of: title, objective, urls, domains, authenticated_sources, query_seeds, include_rules, exclude_rules, match_mode, notification_policy, notify_initial_baseline, schedule."),
        );
    }

    let updated = service
        .update_task(
            scope,
            &task_id,
            UpdateTaskInput {
                title,
                // Set-only, like PATCH /monitors: clearing a schedule is not
                // a monitor operation.
                schedule: schedule.map(Some),
                monitor_spec: spec_touched.then_some(spec),
                ..Default::default()
            },
        )
        .await;
    match updated {
        Ok(task) => {
            // §12 `monitor_updated` — the same shared event seam the PATCH
            // route uses (Phase 6 observability).
            crate::magician_v2::monitor_support::record_monitor_updated_event(
                service, scope, &task,
            )
            .await;
            let parsed = parsed_schedule(task.manifest.schedule.as_ref());
            json!({
                "status": "updated",
                "task_id": task.manifest.task_id,
                "title": task.manifest.title,
                "monitor_revision": task.manifest.monitor_revision,
                "state": monitor_state_label(schedule_is_paused(parsed.as_ref())),
                "cadence_summary": cadence_summary(parsed.as_ref()),
            })
        },
        Err(error) => error_response(format!("update_monitor failed: {error}"), None),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::artifact_v2::{CreateTaskInput, V3ReadApi};
    use crate::magician_v2::test_support::build_test_artifact_v2_service;
    use tempfile::TempDir;

    const PRINCIPAL: &str = "anonymous";
    const WORKSPACE: &str = "default";

    fn scope() -> ScopeRef {
        ScopeRef::system_internal_unauthenticated(&PRINCIPAL.to_string(), &WORKSPACE.to_string())
    }

    fn service() -> (TempDir, Arc<ArtifactV2Service>) {
        let tmp = TempDir::new().expect("tempdir");
        let service = build_test_artifact_v2_service(tmp.path());
        (tmp, service)
    }

    /// Flat args mirroring the YAML schema, aligned with the canonical
    /// Phase 0 spec fixture's field vocabulary.
    fn preview_args() -> Value {
        json!({
            "objective": "Watch the Acme pricing page and tell me when plans or prices change",
            "urls": ["https://acme.example/pricing"],
            "domains": ["acme.example"],
            "query_seeds": ["acme pricing change"],
            "include_rules": ["plan price changes"],
            "exclude_rules": ["blog posts"],
            "match_mode": "balanced",
            "notification_policy": "material_changes",
            "notify_initial_baseline": false,
            "schedule": {
                "kind": { "Cron": { "expression": "0 9 * * 1", "timezone": "UTC" } }
            }
        })
    }

    async fn task_count(service: &Arc<ArtifactV2Service>) -> usize {
        V3ReadApi::list_tasks(service.as_ref(), &scope())
            .await
            .expect("list tasks")
            .len()
    }

    // ── YAML schema round-trip (the three pack defs parse + dispatch) ────

    #[test]
    fn monitor_tool_pack_defs_round_trip_and_are_registered() {
        use crate::magician_v2::execution::capability::CapabilityPackDefinition;
        use crate::magician_v2::execution::capability::ImplementationType;

        for (name, yaml, required) in [
            (
                "preview_monitor",
                include_str!("../embedded_pack_defs/preview_monitor.yaml"),
                vec!["objective"],
            ),
            (
                "create_monitor",
                include_str!("../embedded_pack_defs/create_monitor.yaml"),
                vec!["objective", "preview_fingerprint"],
            ),
            (
                "update_monitor",
                include_str!("../embedded_pack_defs/update_monitor.yaml"),
                vec!["task_id"],
            ),
        ] {
            let def: CapabilityPackDefinition =
                serde_yaml::from_str(yaml).unwrap_or_else(|e| panic!("{name}.yaml parses: {e}"));
            assert_eq!(def.name, name);
            match &def.implementation {
                ImplementationType::Compiled { provider_name } => {
                    assert_eq!(provider_name, name, "{name} dispatches to itself");
                },
                other => panic!("{name} must be a compiled pack, got {other:?}"),
            }
            for param in required {
                assert!(
                    def.parameters
                        .iter()
                        .any(|declared| declared.name == param && declared.required),
                    "{name}.yaml must declare required param `{param}`"
                );
            }
            // The embedded catalog + handler registry both know the tool —
            // the drift that silently strands a pack def (see the
            // COMPILED_PROVIDERS consolidation note) fails here.
            assert!(
                crate::magician_v2::execution::compiled_providers::embedded_compiled_pack_defs()
                    .iter()
                    .any(|pack| pack.name == name),
                "{name} missing from embedded_compiled_pack_defs"
            );
            assert!(
                crate::magician_v2::execution::compiled_providers::default_compiled_handler_registry()
                    .get(name)
                    .is_some(),
                "{name} missing from default_compiled_handler_registry"
            );
        }
    }

    // ── create_task.yaml schedule documentation (C1) ─────────────────────

    /// `AgentScheduleKind` is EXTERNALLY tagged (`{"Cron": {...}}`), and
    /// `monitors_api::parsed_schedule` / the create-task path parse with
    /// `.ok()` — a wrongly-documented internally-tagged example
    /// (`{"type":"Cron",...}`) would make every chat-authored schedule
    /// silently unscheduled. This test pins BOTH that the guide documents
    /// the externally-tagged shape verbatim AND that each documented
    /// example string actually parses as a `TaskSchedule`.
    #[test]
    fn create_task_yaml_schedule_examples_parse_as_task_schedule() {
        let yaml = include_str!("../embedded_pack_defs/create_task.yaml");

        let documented_examples = [
            r#"{ "kind": {"Cron":{"expression":"0 9 * * *","timezone":"America/New_York"}}, "max_runs": 7 }"#,
            r#"{ "kind": {"Cron":{"expression":"*/5 * * * *","timezone":"UTC"}}, "max_runs": 12 }"#,
            r#"{ "kind": {"Interval":{"seconds":300,"jitter_seconds":30}} }"#,
        ];
        for example in documented_examples {
            assert!(
                yaml.contains(example),
                "create_task.yaml guide must document this externally-tagged example verbatim: {example}"
            );
            let value: Value = serde_json::from_str(example)
                .unwrap_or_else(|e| panic!("documented example is not JSON ({e}): {example}"));
            let schedule: TaskSchedule = serde_json::from_value(value).unwrap_or_else(|e| {
                panic!("documented schedule example must parse as TaskSchedule ({e}): {example}")
            });
            // And it survives the exact parse path monitors/create_task use.
            let round_trip = serde_json::to_value(&schedule).expect("schedule serializes");
            assert!(
                parsed_schedule(Some(&round_trip)).is_some(),
                "parsed_schedule must accept the documented shape: {example}"
            );
        }

        // The broken internally-tagged spelling must never come back.
        assert!(
            !yaml.contains(r#""type":"Cron""#) && !yaml.contains(r#""type": "Cron""#),
            "create_task.yaml must not document the internally-tagged {{\"type\":\"Cron\"}} shape — AgentScheduleKind is externally tagged"
        );
        // …and it really does NOT parse, which is why the doc matters.
        let internally_tagged = serde_json::json!({
            "kind": { "type": "Cron", "expression": "0 9 * * *", "timezone": "UTC" }
        });
        assert!(
            serde_json::from_value::<TaskSchedule>(internally_tagged).is_err(),
            "the internally-tagged shape must fail to parse (it silently drops schedules)"
        );
    }

    // ── preview ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn preview_returns_the_contract_and_does_not_create() {
        let (_tmp, service) = service();

        let response = preview_monitor_response(&preview_args());
        assert_eq!(response["status"], "previewed");
        assert_eq!(response["created"], false);
        assert_eq!(
            response["contract"]["spec"]["objective"],
            "Watch the Acme pricing page and tell me when plans or prices change"
        );
        assert_eq!(response["contract"]["spec"]["schema_version"], 1);
        assert_eq!(
            response["contract"]["cadence_summary"], "Cron 0 9 * * 1 (UTC)",
            "preview shows the EXACT schedule summary the list rows use"
        );
        let fingerprint = response["preview_fingerprint"]
            .as_str()
            .expect("fingerprint");
        assert!(fingerprint.starts_with("mpv_"));

        // Deterministic: the same ask previews to the same fingerprint.
        let again = preview_monitor_response(&preview_args());
        assert_eq!(again["preview_fingerprint"].as_str(), Some(fingerprint));

        // NOTHING was created (plan §5.1: preview is review-only).
        assert_eq!(task_count(&service).await, 0);
    }

    #[tokio::test]
    async fn preview_rejects_invalid_specs_with_stable_reasons() {
        // No sources at all.
        let response = preview_monitor_response(&json!({
            "objective": "Watch something"
        }));
        assert_eq!(response["status"], "error");
        assert_eq!(response["reason"], "monitor_sources_required");

        // Missing objective.
        let response = preview_monitor_response(&json!({
            "urls": ["https://acme.example/pricing"]
        }));
        assert_eq!(response["reason"], "monitor_objective_required");

        // Bad URL scheme — the Phase 1 admission gate, verbatim.
        let response = preview_monitor_response(&json!({
            "objective": "Watch",
            "urls": ["javascript:alert(1)"]
        }));
        assert_eq!(response["reason"], "monitor_source_url_scheme_unsupported");

        // Bad schedule shape uses the existing task schedule parser.
        let mut args = preview_args();
        args["schedule"] = json!({ "kind": "not-a-schedule" });
        let response = preview_monitor_response(&args);
        assert_eq!(response["status"], "error");
        assert!(response["reason"]
            .as_str()
            .expect("reason")
            .starts_with("monitor_schedule_invalid"));
    }

    // ── create ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn create_requires_the_previewed_contract() {
        let (_tmp, service) = service();

        // Without a fingerprint: refused, nothing created.
        let response = create_monitor_response(&service, &scope(), &preview_args()).await;
        assert_eq!(response["status"], "error");
        assert_eq!(response["reason"], REASON_PREVIEW_REQUIRED);
        assert_eq!(task_count(&service).await, 0);

        // With a stale fingerprint (args changed after preview): refused.
        let preview = preview_monitor_response(&preview_args());
        let mut edited = preview_args();
        edited["objective"] = json!("Watch a different page");
        edited["preview_fingerprint"] = preview["preview_fingerprint"].clone();
        let response = create_monitor_response(&service, &scope(), &edited).await;
        assert_eq!(response["reason"], REASON_PREVIEW_STALE);
        assert_eq!(task_count(&service).await, 0);
    }

    #[tokio::test]
    async fn create_requires_a_valid_spec_even_with_a_fingerprint() {
        let (_tmp, service) = service();
        let response = create_monitor_response(
            &service,
            &scope(),
            &json!({
                "objective": "Watch nothing in particular",
                "preview_fingerprint": "mpv_0000000000000000"
            }),
        )
        .await;
        assert_eq!(response["status"], "error");
        assert_eq!(response["reason"], "monitor_sources_required");
        assert_eq!(task_count(&service).await, 0);
    }

    #[tokio::test]
    async fn create_with_the_previewed_fingerprint_uses_the_canonical_service_path() {
        let (_tmp, service) = service();

        let preview = preview_monitor_response(&preview_args());
        let mut args = preview_args();
        args["preview_fingerprint"] = preview["preview_fingerprint"].clone();
        let response = create_monitor_response(&service, &scope(), &args).await;
        assert_eq!(response["status"], "created", "response: {response}");
        assert_eq!(
            response["monitor_revision"], 1,
            "first spec write is revision 1 (server-owned)"
        );
        assert_eq!(response["state"], "active");
        assert_eq!(response["cadence_summary"], "Cron 0 9 * * 1 (UTC)");
        let task_id = response["task_id"].as_str().expect("task_id");

        // The created record IS a monitor task on the canonical store: spec
        // attached, revision 1, schedule persisted, title from objective.
        let task = service.get_task(&scope(), task_id).await.expect("task");
        let spec = task.manifest.monitor_spec.expect("monitor_spec attached");
        assert_eq!(
            spec.objective,
            "Watch the Acme pricing page and tell me when plans or prices change"
        );
        assert_eq!(task.manifest.monitor_revision, 1);
        assert!(task.manifest.schedule.is_some());
        assert_eq!(
            task.manifest.title,
            "Watch the Acme pricing page and tell me when plans or prices change"
        );

        // Replaying the same confirmed create mints a SECOND monitor only via
        // an explicit second call — no hidden dedupe surprises here; the
        // duplicate guard lives in chat dispatch for create_task only.
        assert_eq!(task_count(&service).await, 1);
    }

    // ── update ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn update_merges_spec_fields_and_bumps_the_server_owned_revision() {
        let (_tmp, service) = service();
        let preview = preview_monitor_response(&preview_args());
        let mut args = preview_args();
        args["preview_fingerprint"] = preview["preview_fingerprint"].clone();
        let created = create_monitor_response(&service, &scope(), &args).await;
        let task_id = created["task_id"].as_str().expect("task_id").to_string();

        let response = update_monitor_response(
            &service,
            &scope(),
            &json!({
                "task_id": task_id,
                "objective": "Watch the Acme pricing page (weekly digest)",
                "exclude_rules": []
            }),
        )
        .await;
        assert_eq!(response["status"], "updated", "response: {response}");
        assert_eq!(response["monitor_revision"], 2, "spec edit bumps revision");

        let task = service.get_task(&scope(), &task_id).await.expect("task");
        let spec = task.manifest.monitor_spec.expect("spec");
        assert_eq!(
            spec.objective,
            "Watch the Acme pricing page (weekly digest)"
        );
        assert!(spec.exclude_rules.is_empty(), "explicit [] clears the list");
        // Untouched fields survive the merge.
        assert_eq!(spec.sources.urls, vec!["https://acme.example/pricing"]);

        // A schedule/title-only edit does NOT bump the spec revision.
        let response = update_monitor_response(
            &service,
            &scope(),
            &json!({
                "task_id": task_id,
                "title": "Acme pricing watch"
            }),
        )
        .await;
        assert_eq!(response["status"], "updated");
        assert_eq!(response["monitor_revision"], 2);
        assert_eq!(response["title"], "Acme pricing watch");
    }

    #[tokio::test]
    async fn update_rejects_plain_tasks_and_invalid_merges() {
        let (_tmp, service) = service();

        // A plain task is never editable through the monitor tool.
        let plain = service
            .create_task(CreateTaskInput {
                principal: PRINCIPAL.to_string(),
                workspace: WORKSPACE.to_string(),
                title: "Plain task".to_string(),
                description: "Not a monitor".to_string(),
                agent_id: "personal-assistant".to_string(),
                goal_id: None,
                ui_thread_id: "general".to_string(),
                priority: None,
                due_date: None,
                tags: Vec::new(),
                created_by: "user".to_string(),
                depends_on: Vec::new(),
                approved: true,
                schedule: None,
                output_mode: crate::magician_v2::artifact_v2::models::TaskOutputMode::default(),
                chat_session_id: None,
                lifecycle: crate::magician_v2::artifact_v2::models::TaskLifecycle::Persistent,
                sync_mode: crate::magician_v2::artifact_v2::models::TaskSyncMode::default(),
            })
            .await
            .expect("plain task creates");
        let response = update_monitor_response(
            &service,
            &scope(),
            &json!({ "task_id": plain.manifest.task_id, "objective": "hijack" }),
        )
        .await;
        assert_eq!(response["status"], "error");
        assert_eq!(response["reason"], REASON_MONITOR_NOT_FOUND);

        // A merge that empties every source is refused by the admission gate
        // and leaves the stored spec untouched.
        let preview = preview_monitor_response(&preview_args());
        let mut args = preview_args();
        args["preview_fingerprint"] = preview["preview_fingerprint"].clone();
        let created = create_monitor_response(&service, &scope(), &args).await;
        let task_id = created["task_id"].as_str().expect("task_id").to_string();
        let response = update_monitor_response(
            &service,
            &scope(),
            &json!({
                "task_id": task_id,
                "urls": [],
                "domains": [],
                "query_seeds": []
            }),
        )
        .await;
        assert_eq!(response["reason"], "monitor_sources_required");
        let task = service.get_task(&scope(), &task_id).await.expect("task");
        assert_eq!(task.manifest.monitor_revision, 1, "revision unchanged");
        assert!(!task
            .manifest
            .monitor_spec
            .expect("spec")
            .sources
            .urls
            .is_empty());

        // A no-op call is an explicit error, mirroring update_task.
        let response =
            update_monitor_response(&service, &scope(), &json!({ "task_id": task_id })).await;
        assert_eq!(response["status"], "error");
    }
}
