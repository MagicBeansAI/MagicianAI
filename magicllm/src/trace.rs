//! Provider-independent identity carried by logical LLM calls and dispatch jobs.
//!
//! These values are strictly in-process control-plane metadata. Provider
//! adapters may continue forwarding the legacy `RequestMetadata::trace_id`
//! where supported, but must never serialize scope or product lineage fields
//! into an upstream request.

use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// The reserved system scope, mirroring Magician's `SYSTEM_PRINCIPAL` /
/// `SYSTEM_WORKSPACE`. Duplicated rather than imported because magicllm sits
/// below magician and cannot depend on it; the two must not drift.
pub const RESERVED_SYSTEM_PRINCIPAL: &str = "system";
pub const RESERVED_SYSTEM_WORKSPACE: &str = "system";

/// Authoritative local scope for one logical call.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LlmScope {
    pub principal: String,
    pub workspace: String,
}

impl LlmScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
        }
    }

    /// The reserved bucket for calls that reach the trace layer without a
    /// scope of their own — a clean shutdown flush, and compatibility callers
    /// that predate typed scope.
    ///
    /// This is deliberately the reserved system scope and NOT `system`/
    /// `default`. That pairing looked reserved but belonged to nothing: the
    /// system scope is `system`/`system` and the default user scope is
    /// `anonymous`/`default`, so `system`/`default` was a hybrid that named
    /// neither. Because a scope is created implicitly by anything that writes
    /// to it, those unscoped rows manufactured a whole phantom scope on disk —
    /// which boot-time enumeration then discovered and initialised, giving it
    /// all ~17 subsystem directories, a duplicate copy of every seed app
    /// package, and a permanent slot in every per-scope sweep. Measured
    /// 2026-09-15: ~12 MB and a 60-second bootstrap pass, sustaining 8 KB of
    /// real rows.
    ///
    /// `LlmScopeResolution::LegacyDefault` still marks *how* the scope was
    /// resolved, so these calls stay greppable in analytics; only where they
    /// land has changed.
    pub fn legacy_default() -> Self {
        Self::new(RESERVED_SYSTEM_PRINCIPAL, RESERVED_SYSTEM_WORKSPACE)
    }

    pub fn is_valid(&self) -> bool {
        is_safe_scope_component(&self.principal) && is_safe_scope_component(&self.workspace)
    }
}

/// Scope values become filesystem path components in Magician's durable
/// analytics layout. Reject values that path projection would rewrite:
/// accepting both `team/a` and `team_a` would make two logical scopes share
/// one on-disk tenant boundary.
fn is_safe_scope_component(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed == value
        && value.len() <= 255
        && value != "."
        && value != ".."
        && !value.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
        })
}

/// How a call acquired its local scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmScopeResolution {
    Explicit,
    Inherited,
    SystemDefault,
    LegacyDefault,
}

impl LlmScopeResolution {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Inherited => "inherited",
            Self::SystemDefault => "system_default",
            Self::LegacyDefault => "legacy_default",
        }
    }
}

/// Broad workload class used for priority and later outcome interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmWorkloadClass {
    ForegroundChat,
    InteractiveTask,
    AutonomousTask,
    Scheduled,
    Ambient,
    CommsAssist,
    Memory,
    Evaluation,
    System,
}

impl Default for LlmWorkloadClass {
    fn default() -> Self {
        Self::System
    }
}

impl LlmWorkloadClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ForegroundChat => "foreground_chat",
            Self::InteractiveTask => "interactive_task",
            Self::AutonomousTask => "autonomous_task",
            Self::Scheduled => "scheduled",
            Self::Ambient => "ambient",
            Self::CommsAssist => "comms_assist",
            Self::Memory => "memory",
            Self::Evaluation => "evaluation",
            Self::System => "system",
        }
    }
}

/// Relationship between a child call and its logical parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmParentRelation {
    Cascade,
    Verifies,
    Judges,
    Summarizes,
    Supports,
    ChunkMap,
    ChunkRepair,
    ChunkFallback,
    ChunkReduce,
}

impl LlmParentRelation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cascade => "cascade",
            Self::Verifies => "verifies",
            Self::Judges => "judges",
            Self::Summarizes => "summarizes",
            Self::Supports => "supports",
            Self::ChunkMap => "chunk_map",
            Self::ChunkRepair => "chunk_repair",
            Self::ChunkFallback => "chunk_fallback",
            Self::ChunkReduce => "chunk_reduce",
        }
    }
}

/// Functional role of one logical call in a larger product operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmCallRole {
    Primary,
    Supporting,
    Summarizer,
    Classifier,
    Validator,
    Verifier,
    Judge,
    Recovery,
    Title,
    Memory,
}

impl Default for LlmCallRole {
    fn default() -> Self {
        Self::Primary
    }
}

impl LlmCallRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Supporting => "supporting",
            Self::Summarizer => "summarizer",
            Self::Classifier => "classifier",
            Self::Validator => "validator",
            Self::Verifier => "verifier",
            Self::Judge => "judge",
            Self::Recovery => "recovery",
            Self::Title => "title",
            Self::Memory => "memory",
        }
    }
}

/// Stable identity and product lineage for one logical model request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmTraceContext {
    pub trace_id: String,
    pub llm_call_id: String,
    /// The runtime activity span this request was issued under, when it was
    /// issued under one.
    ///
    /// The join key between an analytical LLM record and the live activity
    /// view. `LlmCallCompleted` already carries the price and the tokens; this
    /// is what says *which unit of work* they belong to, and without it the two
    /// can only be correlated by guesswork.
    ///
    /// Deliberately carried here as well as on `JobOrigin` rather than reached
    /// through it. The dispatch sink reads the origin; the canonical lifecycle
    /// recorder sees only this context, and making the recorder chase the
    /// origin would leave two paths that disagree the moment either changes.
    /// Both are stamped from the same source — the layer's
    /// `current_activity_id()`, read where the span is still live — so they
    /// cannot drift apart in value, only in presence.
    ///
    /// `None` is ordinary and expected: a model request issued outside any
    /// instrumented span has no activity to belong to, and inventing one would
    /// attach cost to unrelated work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_relation: Option<LlmParentRelation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_decision_id: Option<String>,
    pub scope: LlmScope,
    pub scope_resolution: LlmScopeResolution,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_message_id: Option<String>,
    pub workload_class: LlmWorkloadClass,
    #[serde(default)]
    pub call_role: LlmCallRole,
}

impl LlmTraceContext {
    pub fn new(scope: LlmScope, workload_class: LlmWorkloadClass) -> Self {
        let root = Ulid::new().to_string();
        Self {
            trace_id: root.clone(),
            llm_call_id: Ulid::new().to_string(),
            parent_call_id: None,
            parent_relation: None,
            retry_group_id: None,
            route_decision_id: None,
            scope,
            scope_resolution: LlmScopeResolution::Explicit,
            task_id: None,
            root_execution_id: None,
            execution_id: None,
            plan_id: None,
            step_id: None,
            iteration_id: None,
            chat_session_id: None,
            chat_turn_id: None,
            user_message_id: None,
            workload_class,
            call_role: LlmCallRole::Primary,
            // Stamped by the caller after construction, where the activity
            // span is still live. `magicllm` cannot read it here: the span
            // registry lives in `magician`, above this crate.
            activity_id: None,
        }
    }

    /// Compatibility constructor for callers not yet supplying typed scope.
    /// A legacy trace id stays a root trace id and is never reused as the new
    /// logical call id.
    pub fn legacy(trace_id: Option<&str>, workload_class: LlmWorkloadClass) -> Self {
        let mut context = Self::new(LlmScope::legacy_default(), workload_class);
        context.scope_resolution = LlmScopeResolution::LegacyDefault;
        if let Some(trace_id) = trace_id.map(str::trim).filter(|value| !value.is_empty()) {
            context.trace_id = trace_id.to_string();
        }
        context
    }

    pub fn with_scope_resolution(mut self, resolution: LlmScopeResolution) -> Self {
        self.scope_resolution = resolution;
        self
    }

    pub fn child(&self, relation: LlmParentRelation, role: LlmCallRole) -> Self {
        let mut child = self.clone();
        child.llm_call_id = Ulid::new().to_string();
        child.parent_call_id = Some(self.llm_call_id.clone());
        child.parent_relation = Some(relation);
        // Retry groups describe caller retries of one logical call. A child is
        // a new logical call and must not accidentally inherit its parent's
        // retry group; retrying this child later will establish its own group.
        child.retry_group_id = None;
        child.scope_resolution = LlmScopeResolution::Inherited;
        child.call_role = role;
        child
    }

    /// Caller-level retry: a new logical call under one stable retry group.
    pub fn caller_retry(&self) -> Self {
        let mut retry = self.clone();
        retry.llm_call_id = Ulid::new().to_string();
        // Preserve parent lineage when the retried call is itself a child
        // (validator, reducer, cascade, and so on). Clearing this edge turns a
        // child retry into an unrelated root call and breaks attribution.
        retry.retry_group_id = Some(
            self.retry_group_id
                .clone()
                .unwrap_or_else(|| self.llm_call_id.clone()),
        );
        retry.scope_resolution = LlmScopeResolution::Inherited;
        retry
    }

    pub fn provider_attempt_id(&self, ordinal: u32) -> String {
        format!("{}:a{}", self.llm_call_id, ordinal)
    }

    /// Stamp the activity this call is being issued under, dropping a value
    /// that could not be recorded.
    ///
    /// The normalising setter exists because the field is `pub` and the check
    /// that used to catch a bad value has been removed from [`Self::is_valid`]
    /// — see the note there. Callers reaching a live span should stamp through
    /// here rather than assigning the field.
    pub fn set_activity_id<S: AsRef<str>>(&mut self, activity_id: Option<S>) {
        self.activity_id = normalized_activity_id(activity_id);
    }

    /// Whether this context may identify a call.
    ///
    /// **Admission depends on this**, not just telemetry quality:
    /// `ConfiguredRouter::route` turns `false` into a `Validation` error and the
    /// model call is refused. Only fields whose absence or corruption makes the
    /// call *unidentifiable* belong here.
    ///
    /// `activity_id` deliberately does not. It is a join key from a call to the
    /// live activity row that issued it, and `None` is documented as ordinary —
    /// a call outside any instrumented span has no activity to belong to. A
    /// malformed one is a telemetry defect of exactly that shape, one degree
    /// worse; converting it into a refused LLM call trades a missing join key
    /// for lost work. It was in this list, and today's only producer is a `u64`
    /// so nothing could trip it — but `JobOrigin::with_activity_id` is `pub`,
    /// and the first caller to pass `Some("")` would have had its request
    /// rejected outright. **No field that exists only to describe a call may
    /// refuse one.**
    ///
    /// The value is still held to the same bar; it is enforced where it enters
    /// instead ([`normalized_activity_id`], used by
    /// `JobOrigin::with_activity_id`, `LlmJob::new` and the analytics
    /// reconstruction), so a malformed id becomes `None` and costs the join key
    /// alone.
    pub fn is_valid(&self) -> bool {
        is_stable_identifier(&self.trace_id)
            && is_stable_identifier(&self.llm_call_id)
            && self.scope.is_valid()
            && self.parent_call_id.is_some() == self.parent_relation.is_some()
            && self
                .parent_call_id
                .as_ref()
                .is_none_or(|parent| is_stable_identifier(parent) && parent != &self.llm_call_id)
            && self.retry_group_id.as_ref().is_none_or(|retry_group| {
                is_stable_identifier(retry_group) && retry_group != &self.llm_call_id
            })
            && [
                self.route_decision_id.as_deref(),
                self.task_id.as_deref(),
                self.root_execution_id.as_deref(),
                self.execution_id.as_deref(),
                self.plan_id.as_deref(),
                self.step_id.as_deref(),
                self.iteration_id.as_deref(),
                self.chat_session_id.as_deref(),
                self.chat_turn_id.as_deref(),
                self.user_message_id.as_deref(),
            ]
            .into_iter()
            .all(|value| value.is_none_or(is_stable_identifier))
    }

    pub fn same_scope(&self, other: &Self) -> bool {
        self.scope == other.scope
    }
}

fn is_stable_identifier(value: &str) -> bool {
    !value.is_empty() && value.trim() == value
}

/// Coerce a caller-supplied activity id into one that can be recorded, or into
/// nothing at all.
///
/// The boundary where a telemetry defect is contained. An id that is empty or
/// whitespace-only becomes `None`, which is a state every consumer already
/// handles: `emit_activity_cost` emits nothing, and the parquet column is
/// nullable. A padded id is trimmed rather than dropped — the same repair
/// [`LlmTraceContext::legacy`] applies to a supplied trace id — because the
/// padding is the accident, not the identity.
///
/// The alternative was to keep rejecting a bad id in [`LlmTraceContext::is_valid`],
/// which is checked before a request is admitted: that turns a wrong *label* on
/// some work into that work not happening.
pub fn normalized_activity_id<S: AsRef<str>>(activity_id: Option<S>) -> Option<String> {
    activity_id
        .map(|value| value.as_ref().trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Correlation receipt returned after a direct or queued model call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmTraceReceipt {
    pub context: LlmTraceContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_attempt_id: Option<String>,
    #[serde(default)]
    pub provider_attempt_count: u32,
    #[serde(default)]
    pub response_reused: bool,
}

impl LlmTraceReceipt {
    pub fn direct(context: LlmTraceContext) -> Self {
        Self::direct_with_attempt_count(context, 1)
    }

    pub fn direct_with_attempt_count(
        context: LlmTraceContext,
        provider_attempt_count: u32,
    ) -> Self {
        Self {
            provider_attempt_id: (provider_attempt_count > 0)
                .then(|| context.provider_attempt_id(provider_attempt_count)),
            context,
            dispatch_job_id: None,
            provider_attempt_count,
            response_reused: false,
        }
    }

    pub fn queued(
        context: LlmTraceContext,
        dispatch_job_id: impl Into<String>,
        provider_attempt_count: u32,
    ) -> Self {
        Self {
            provider_attempt_id: (provider_attempt_count > 0)
                .then(|| context.provider_attempt_id(provider_attempt_count)),
            context,
            dispatch_job_id: Some(dispatch_job_id.into()),
            provider_attempt_count,
            response_reused: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_trace_is_not_reused_as_call_id() {
        let context = LlmTraceContext::legacy(Some("existing-trace"), LlmWorkloadClass::System);
        assert_eq!(context.trace_id, "existing-trace");
        assert_ne!(context.llm_call_id, "existing-trace");
        assert!(context.is_valid());
    }

    #[test]
    fn stable_identity_rejects_blank_or_boundary_whitespace() {
        let baseline =
            LlmTraceContext::new(LlmScope::new("owner", "default"), LlmWorkloadClass::System);
        for field in ["trace_id", "llm_call_id", "parent_call_id", "task_id"] {
            let mut context = baseline.clone();
            match field {
                "trace_id" => context.trace_id = " padded".to_string(),
                "llm_call_id" => context.llm_call_id = "padded ".to_string(),
                "parent_call_id" => {
                    context.parent_call_id = Some(" parent ".to_string());
                    context.parent_relation = Some(LlmParentRelation::Supports);
                },
                "task_id" => context.task_id = Some(" ".to_string()),
                _ => unreachable!(),
            }
            assert!(!context.is_valid(), "{field} must be canonical");
        }
    }

    #[test]
    fn child_and_retry_have_distinct_semantics() {
        let root = LlmTraceContext::new(
            LlmScope::new("owner", "default"),
            LlmWorkloadClass::InteractiveTask,
        );
        let child = root.child(LlmParentRelation::Verifies, LlmCallRole::Verifier);
        let retry = root.caller_retry();
        assert_eq!(
            child.parent_call_id.as_deref(),
            Some(root.llm_call_id.as_str())
        );
        assert_eq!(child.trace_id, root.trace_id);
        assert_eq!(
            retry.retry_group_id.as_deref(),
            Some(root.llm_call_id.as_str())
        );
        assert_ne!(retry.llm_call_id, root.llm_call_id);
        assert!(root.same_scope(&child));
    }

    #[test]
    fn caller_retry_gets_new_call_id_and_stable_retry_group() {
        let original = LlmTraceContext::new(
            LlmScope::new("principal", "workspace"),
            LlmWorkloadClass::ForegroundChat,
        );
        let retry_one = original.caller_retry();
        let retry_two = retry_one.caller_retry();

        assert_eq!(retry_one.trace_id, original.trace_id);
        assert_eq!(retry_two.trace_id, original.trace_id);
        assert_ne!(retry_one.llm_call_id, original.llm_call_id);
        assert_ne!(retry_two.llm_call_id, retry_one.llm_call_id);
        assert_eq!(
            retry_one.retry_group_id.as_deref(),
            Some(original.llm_call_id.as_str())
        );
        assert_eq!(retry_two.retry_group_id, retry_one.retry_group_id);
        assert!(retry_one.same_scope(&original));
        assert!(retry_two.same_scope(&original));
    }

    #[test]
    fn retrying_a_child_preserves_parent_lineage_and_uses_a_child_retry_group() {
        let root = LlmTraceContext::new(
            LlmScope::new("principal", "workspace"),
            LlmWorkloadClass::InteractiveTask,
        );
        let child = root.child(LlmParentRelation::Verifies, LlmCallRole::Verifier);
        let retry = child.caller_retry();

        assert_eq!(retry.parent_call_id, child.parent_call_id);
        assert_eq!(retry.parent_relation, child.parent_relation);
        assert_eq!(
            retry.retry_group_id.as_deref(),
            Some(child.llm_call_id.as_str())
        );
        assert_ne!(retry.llm_call_id, child.llm_call_id);
    }

    #[test]
    fn child_does_not_inherit_an_unrelated_parent_retry_group() {
        let original = LlmTraceContext::new(
            LlmScope::new("principal", "workspace"),
            LlmWorkloadClass::InteractiveTask,
        );
        let retried_parent = original.caller_retry();
        let child = retried_parent.child(LlmParentRelation::Supports, LlmCallRole::Supporting);

        assert!(child.retry_group_id.is_none());
        assert_eq!(
            child.parent_call_id.as_deref(),
            Some(retried_parent.llm_call_id.as_str())
        );
    }

    #[test]
    fn context_rejects_unpaired_parent_edges_and_blank_optional_identity() {
        let mut context = LlmTraceContext::new(
            LlmScope::new("principal", "workspace"),
            LlmWorkloadClass::System,
        );
        context.parent_call_id = Some("parent".to_string());
        assert!(!context.is_valid());
        context.parent_relation = Some(LlmParentRelation::Supports);
        assert!(context.is_valid());
        context.chat_turn_id = Some("  ".to_string());
        assert!(!context.is_valid());
    }

    #[test]
    fn scope_rejects_aliasing_and_unsafe_path_components() {
        for invalid in ["", " owner", "owner ", ".", "..", "team/a", "team_a:prod"] {
            assert!(
                !LlmScope::new(invalid, "workspace").is_valid(),
                "scope component {invalid:?} must be rejected"
            );
        }
        assert!(LlmScope::new("owner@example.com", "workspace-1").is_valid());
    }

    /// A telemetry field must not be able to refuse an LLM call.
    ///
    /// `is_valid` is not a quality score: `ConfiguredRouter::route` turns
    /// `false` into `LLMError::Validation` and the model is never called. While
    /// `activity_id` was in the checked list, a caller passing `Some("")` to the
    /// `pub` `JobOrigin::with_activity_id` would have converted a wrong label
    /// into work that does not happen. Nothing could reach it today — the only
    /// producer stringifies a `u64` — which is exactly why it needs a test
    /// rather than an observation.
    #[test]
    fn a_malformed_activity_id_degrades_telemetry_and_never_refuses_a_call() {
        let baseline =
            LlmTraceContext::new(LlmScope::new("owner", "default"), LlmWorkloadClass::System);
        for malformed in ["", " ", "\t\n", " padded", "padded "] {
            let mut context = baseline.clone();
            context.activity_id = Some(malformed.to_string());
            assert!(
                context.is_valid(),
                "activity id {malformed:?} must not make a request unroutable"
            );
        }

        // Rejected for telemetry all the same, at the boundary rather than the
        // gate: an unusable id becomes absent, and a padded one is repaired.
        for empty in ["", " ", "\t\n"] {
            let mut context = baseline.clone();
            context.set_activity_id(Some(empty));
            assert_eq!(
                context.activity_id, None,
                "an unusable activity id {empty:?} must be dropped, not carried as present"
            );
        }
        let mut context = baseline.clone();
        context.set_activity_id(Some(" 4242 "));
        assert_eq!(context.activity_id.as_deref(), Some("4242"));
        context.set_activity_id(None::<&str>);
        assert_eq!(context.activity_id, None);
    }

    #[test]
    fn zero_attempt_receipt_remains_explicitly_unattempted() {
        let context = LlmTraceContext::new(
            LlmScope::new("principal", "workspace"),
            LlmWorkloadClass::System,
        );
        let direct = LlmTraceReceipt::direct_with_attempt_count(context.clone(), 0);
        let queued = LlmTraceReceipt::queued(context, "job", 0);

        assert_eq!(direct.provider_attempt_count, 0);
        assert!(direct.provider_attempt_id.is_none());
        assert_eq!(queued.provider_attempt_count, 0);
        assert!(queued.provider_attempt_id.is_none());
    }

    #[test]
    fn logical_call_change_resets_attempt_counter_but_lineage_enrichment_does_not() {
        let original = LlmTraceContext::new(
            LlmScope::new("owner", "default"),
            LlmWorkloadClass::ForegroundChat,
        );
        let mut metadata = crate::types::RequestMetadata::default();
        metadata.set_trace_context(original.clone());
        assert_eq!(metadata.record_provider_attempt(), 1);

        let mut enriched = original.clone();
        enriched.chat_turn_id = Some("turn-1".to_string());
        metadata.set_trace_context(enriched.clone());
        assert_eq!(metadata.record_provider_attempt(), 2);

        metadata.set_trace_context(
            enriched.child(LlmParentRelation::Supports, LlmCallRole::Supporting),
        );
        assert_eq!(metadata.provider_attempt_count(), 0);
        assert_eq!(metadata.record_provider_attempt(), 1);
    }

    #[test]
    fn self_referencing_retry_group_is_invalid() {
        let mut context = LlmTraceContext::new(
            LlmScope::new("owner", "default"),
            LlmWorkloadClass::InteractiveTask,
        );
        context.retry_group_id = Some(context.llm_call_id.clone());

        assert!(!context.is_valid());
    }

    #[test]
    fn chat_tool_loop_uses_distinct_child_calls_under_one_trace() {
        let mut primary = LlmTraceContext::new(
            LlmScope::new("principal", "workspace"),
            LlmWorkloadClass::ForegroundChat,
        );
        primary.chat_session_id = Some("session-1".to_string());
        primary.chat_turn_id = Some("turn-1".to_string());
        let tool_result_followup = primary.child(LlmParentRelation::Supports, LlmCallRole::Primary);

        assert_eq!(tool_result_followup.trace_id, primary.trace_id);
        assert_eq!(
            tool_result_followup.parent_call_id.as_deref(),
            Some(primary.llm_call_id.as_str())
        );
        assert_eq!(
            tool_result_followup.chat_session_id,
            primary.chat_session_id
        );
        assert_eq!(tool_result_followup.chat_turn_id, primary.chat_turn_id);
        assert_ne!(tool_result_followup.llm_call_id, primary.llm_call_id);
    }

    #[test]
    fn quality_cascade_is_a_child_with_the_same_route_decision() {
        let mut primary = LlmTraceContext::new(
            LlmScope::new("principal", "workspace"),
            LlmWorkloadClass::InteractiveTask,
        );
        primary.route_decision_id = Some("route-1".to_string());
        let cascade = primary.child(LlmParentRelation::Cascade, LlmCallRole::Recovery);

        assert_eq!(cascade.trace_id, primary.trace_id);
        assert_eq!(
            cascade.parent_call_id.as_deref(),
            Some(primary.llm_call_id.as_str())
        );
        assert_eq!(cascade.parent_relation, Some(LlmParentRelation::Cascade));
        assert_eq!(cascade.route_decision_id.as_deref(), Some("route-1"));
        assert_eq!(cascade.call_role, LlmCallRole::Recovery);
    }

    #[test]
    fn physical_attempt_ids_are_deterministic_and_unique() {
        let context = LlmTraceContext::legacy(None, LlmWorkloadClass::System);
        let attempt_ids = (1..=5)
            .map(|ordinal| context.provider_attempt_id(ordinal))
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(attempt_ids.len(), 5);
        assert!(attempt_ids.contains(&format!("{}:a1", context.llm_call_id)));
        assert!(attempt_ids.contains(&format!("{}:a5", context.llm_call_id)));
    }

    #[test]
    fn request_without_product_scope_is_a_current_system_default_not_legacy_data() {
        let mut metadata = crate::types::RequestMetadata::default();
        let context = metadata.ensure_trace_context(None, LlmWorkloadClass::Memory);
        assert_eq!(context.scope, LlmScope::legacy_default());
        assert_eq!(context.scope_resolution, LlmScopeResolution::SystemDefault);
        assert_eq!(context.workload_class, LlmWorkloadClass::Memory);
    }

    /// A scope is created implicitly by anything that writes to it, so the
    /// fallback must name a scope that is supposed to exist. `system`/`default`
    /// named neither the system scope nor the default user scope, and the rows
    /// that landed there manufactured a phantom scope on disk complete with a
    /// duplicate of every seed app package.
    #[test]
    fn the_unscoped_fallback_is_the_reserved_system_scope_not_a_hybrid() {
        let scope = LlmScope::legacy_default();
        assert_eq!(scope.principal, "system");
        assert_eq!(scope.workspace, "system");
        assert_ne!(
            scope,
            LlmScope::new("system", "default"),
            "system/default is a hybrid of the system principal and the default \
             user workspace; it belongs to neither and materialises a phantom scope"
        );
        assert!(scope.is_valid());
    }
}
