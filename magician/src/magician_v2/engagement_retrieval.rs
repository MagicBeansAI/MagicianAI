//! Engagement-scoped containment at the runtime's retrieval boundaries.
//!
//! OPC Workstream B, §5A.2 of
//! `docs/plans/2026-08-07-opc-engagements-contextual-authority.md`.
//!
//! [`crate::magician_v2::engagements`] is the authority carrier: it says what
//! an execution bound to an engagement may *do*.
//! [`magician_vector_index::retrieval_scope`] is the containment rule: given
//! one stored item's label, it says whether a bound execution may *see* it.
//! This module is the seam between them — it turns the carrier an execution
//! holds into the scope its retrieval runs under, and names the corpora that
//! carry no engagement labels at all so a bound execution is refused rather
//! than quietly handed all of them.
//!
//! # Where the scope comes from, and where it never comes from
//!
//! One source only: the [`EngagementAuthorityRef`] the execution carries,
//! inherited verbatim from the parent's durable `ExecutionRun`. The autonomous
//! dispatch boundary already strips every model-supplied `__*` key before
//! stamping the runtime's own `__engagement_id`
//! (`execution::agentic::executor`), which is what makes
//! [`retrieval_scope_from_runtime_args`] safe to read: by the time a compiled
//! handler sees that key, the model cannot have written it.
//!
//! A child execution cannot supply its own engagement, and neither can a tool
//! argument. An agent that could name its own retrieval scope could name the
//! one whose corpus it wanted.
//!
//! # Two different answers for two different corpora
//!
//! Memory is **labelled and filtered**: entries carry an `engagement_scope`
//! key, and a bound execution retrieves those matching its engagement plus
//! those explicitly marked neutral.
//!
//! Notes, files, tasks, artifacts, execution history and acquired web content
//! carry **no engagement label at all**. There is nothing to filter on, so the
//! honest answer for a bound execution is a refusal, not a filtered read that
//! silently returns everything. That is [`cross_engagement_corpus_refusal`].

use magician_vector_index::retrieval_scope::RetrievalScope;
use serde_json::Value;

use crate::magician_v2::engagements::EngagementAuthorityRef;

/// Runtime-owned dispatch argument carrying the execution's engagement id.
/// Stamped by the autonomous dispatch boundary after model-supplied `__*`
/// keys are stripped.
pub const ENGAGEMENT_ID_ARG: &str = "__engagement_id";

/// The scope an execution carrying `authority` retrieves under.
///
/// `None` is [`RetrievalScope::Unbound`] and that is not a permissive default
/// being invented here: an execution that carries no engagement was never
/// narrowed by one, and narrowing it would delete the owner's own memory from
/// the owner's own chat. The fail-closed rule lives one level down, inside a
/// bound scope, where an item that cannot prove which engagement it belongs to
/// is refused.
///
/// A carrier holding a blank engagement id is refused rather than downgraded:
/// see [`RetrievalScope::bound`]. A blank id would produce a scope that
/// matches no label while looking like containment.
pub fn retrieval_scope_from_authority(
    authority: Option<&EngagementAuthorityRef>,
) -> Result<RetrievalScope, String> {
    let Some(authority) = authority else {
        return Ok(RetrievalScope::Unbound);
    };
    RetrievalScope::bound(&authority.engagement_id).ok_or_else(|| {
        "engagement authority carries a blank engagement id, so retrieval cannot be contained"
            .to_string()
    })
}

/// The scope a compiled handler runs under, read from its runtime-owned
/// dispatch arguments.
///
/// Absence of [`ENGAGEMENT_ID_ARG`] means the dispatching surface carries no
/// engagement — today that is chat (which does not thread engagement refs; see
/// `chat::service`) and any autonomous execution whose `ExecutionRun` has no
/// authority. A present-but-unusable value (non-string, blank) is an error
/// rather than an unbound read, because a surface that meant to bind and
/// failed must not silently retrieve everything.
pub fn retrieval_scope_from_runtime_args(args: &Value) -> Result<RetrievalScope, String> {
    let Some(raw) = args.get(ENGAGEMENT_ID_ARG) else {
        return Ok(RetrievalScope::Unbound);
    };
    if raw.is_null() {
        return Ok(RetrievalScope::Unbound);
    }
    let Some(engagement_id) = raw.as_str() else {
        return Err(format!(
            "`{ENGAGEMENT_ID_ARG}` is present but is not a string, so the engagement this \
             execution retrieves under cannot be read"
        ));
    };
    RetrievalScope::bound(engagement_id).ok_or_else(|| {
        format!("`{ENGAGEMENT_ID_ARG}` is present but blank, so retrieval cannot be contained")
    })
}

/// The scope a context render must run under, or `None` when there isn't one.
///
/// `None` means **render nothing**, and it is the whole reason this returns an
/// `Option` rather than a scope. [`retrieval_scope_from_authority`] fails for a
/// carrier the runtime cannot read, and a render site has no way to refuse the
/// turn — so it needs an answer. Manufacturing a scope for it would be worse
/// than useless: a `Bound` scope with a nonsense id still admits everything
/// labelled `neutral`, so the "safe fallback" would quietly retrieve the one
/// class of material an author marked shareable, on behalf of an engagement
/// nobody could identify.
///
/// So the answer is the absence of a scope, and the caller renders no memory
/// at all. A caller that CAN refuse the act outright — a dispatch gate — uses
/// [`retrieval_scope_from_authority`] and refuses instead.
pub fn contained_retrieval_scope(
    authority: Option<&EngagementAuthorityRef>,
) -> Option<RetrievalScope> {
    retrieval_scope_from_authority(authority).ok()
}

/// A retrieval capability whose corpus has no engagement labels.
///
/// The list is deliberately explicit and deliberately short. Each entry is a
/// tool that reads a store the owner shares across all of their work, so under
/// a bound execution every one of them is a cross-engagement read with no
/// filter available.
///
/// Ordered as it reads in the refusal message.
const UNLABELLED_CORPUS_CAPABILITIES: &[(&str, &str)] = &[
    ("search_notes", "the owner's notes"),
    ("open_note", "the owner's notes"),
    ("append_note", "the owner's notes"),
    ("save_selection_to_note", "the owner's notes"),
    ("read_file", "the owner's filesystem"),
    ("glob", "the owner's filesystem"),
    ("grep", "the owner's filesystem"),
    ("files", "the owner's filesystem"),
    ("list_tasks", "the owner's task history"),
    ("get_task_details", "the owner's task history"),
    ("get_execution_history", "the owner's execution history"),
    ("get_active_executions", "the owner's execution history"),
    ("list_artifacts", "the owner's artifacts"),
    ("app_data_query", "connected app data"),
    ("app_data_search", "connected app data"),
    ("content_search", "previously acquired content"),
    ("content_read", "previously acquired content"),
];

/// Refuse a retrieval that a bound execution cannot contain.
///
/// Returns the refusal text, or `None` when the act may proceed.
///
/// # Why a refusal and not a filter
///
/// Filtering requires a label to filter on. These corpora have none, so a
/// "filtered" read of them would return the whole store and report success —
/// the vacuous-truth failure §5A.2 names. A refusal is the only answer that is
/// true. The engagement's tool ceiling is the intended long-term shape (an
/// outward actor's grant simply does not contain these tools), but the ceiling
/// is enforced today only on acts classified as outward, so an inward read
/// reaches the corpus with nothing in the way.
///
/// # Why an allowlist is not used here
///
/// The complement of this list is not "safe" — it is "not a retrieval over an
/// owner-wide corpus", which includes the outward acts an engagement exists to
/// perform. Refusing everything unlisted would refuse the ambassador's own
/// job. The residual is real and is recorded in the §5A.2 docs: a retrieval
/// capability added later, and not added here, is not contained by this gate.
pub fn cross_engagement_corpus_refusal(capability: &str, scope: &RetrievalScope) -> Option<String> {
    let (binding_kind, binding_id) = scope_binding(scope)?;
    let capability_key = capability.trim().to_ascii_lowercase();
    let (_, corpus) = UNLABELLED_CORPUS_CAPABILITIES
        .iter()
        .find(|(name, _)| *name == capability_key)?;
    Some(format!(
        "NOT RETRIEVED — this execution is confined to {binding_kind} `{binding_id}` and \
         `{capability}` reads {corpus}, which carries no {binding_kind} labels. Nothing there can \
         be shown to belong to this {binding_kind}, and an unlabelled read is a read of every \
         {binding_kind} at once. Use memory recall, which is scope-filtered, or ask the owner."
    ))
}

/// The binding a scope confines to, as (what kind of binding, its id).
///
/// Both bound forms answer, because both confine. Reading only
/// [`RetrievalScope::bound_engagement_id`] here would leave a meeting-confined
/// execution outside this gate entirely — the room would be the one place the
/// owner's notes stayed reachable.
fn scope_binding(scope: &RetrievalScope) -> Option<(&'static str, &str)> {
    if let Some(engagement_id) = scope.bound_engagement_id() {
        return Some(("engagement", engagement_id));
    }
    scope
        .bound_meeting_id()
        .map(|meeting_id| ("meeting", meeting_id))
}

/// A retrieval capability whose corpus carries an ENGAGEMENT label and no
/// program label.
///
/// Deliberately separate from [`UNLABELLED_CORPUS_CAPABILITIES`], and the
/// separation is the whole point: these corpora ARE filterable, just not by a
/// program. Under an engagement, `search_memory` and `forget_memory` are
/// contained by the `__engagement_id` the runtime stamps, and the memory
/// records carry a matching `engagement_scope`. Nothing labels a memory record
/// by program.
///
/// So the same read that is safely narrowed for one kind of work is a read of
/// the owner's entire corpus for the other — and it fails OPEN, which is what
/// makes it worth its own list. A program stamps no scope argument,
/// [`retrieval_scope_from_runtime_args`] answers `Unbound` for an absent key,
/// and `RetrievalScope::decide` admits everything. Refusing to render the
/// corpus into the prompt and then handing the same corpus over through a tool
/// call is the §5A.2 leak in its widest form.
const ENGAGEMENT_LABELLED_ONLY_CAPABILITIES: &[(&str, &str)] = &[
    ("search_memory", "the owner's memory"),
    ("forget_memory", "the owner's memory"),
];

/// The capabilities in [`ENGAGEMENT_LABELLED_ONLY_CAPABILITIES`], by name.
pub fn engagement_labelled_only_capabilities() -> Vec<&'static str> {
    ENGAGEMENT_LABELLED_ONLY_CAPABILITIES
        .iter()
        .map(|(name, _)| *name)
        .collect()
}

/// What [`ENGAGEMENT_LABELLED_ONLY_CAPABILITIES`] says this capability reads,
/// for a refusal that names the corpus rather than only the tool.
pub fn engagement_labelled_only_corpus(capability: &str) -> Option<&'static str> {
    ENGAGEMENT_LABELLED_ONLY_CAPABILITIES
        .iter()
        .find(|(name, _)| *name == capability)
        .map(|(_, corpus)| *corpus)
}

/// Every capability [`cross_engagement_corpus_refusal`] refuses under a bound
/// execution, for tests and for the operator-facing documentation to enumerate
/// from one source.
pub fn unlabelled_corpus_capabilities() -> Vec<&'static str> {
    UNLABELLED_CORPUS_CAPABILITIES
        .iter()
        .map(|(name, _)| *name)
        .collect()
}

/// Name of the native `files` capability, dispatched as its own action shape
/// rather than as a named pack. Kept here so the gate and the table above
/// cannot drift apart on the spelling.
pub const NATIVE_FILES_CAPABILITY: &str = "files";

/// Prefix that partitions an agent-browser session id by retrieval binding.
/// The binding kind is spelled out after it so an engagement's namespace and a
/// meeting's can never collide on a shared id.
const BROWSER_SESSION_SCOPE_PREFIX: &str = "scope";

/// Partition a browser session id by engagement.
///
/// The agent-browser CLI keys a launched browser's state — its cookies, its
/// logged-in accounts, its storage — by `--session <id>`. Two executions that
/// resolve the same session id share one browser. So the session id is where
/// an engagement boundary can be drawn without a new CLI flag: give each
/// engagement its own id space and no bound execution can ever land in a
/// window another engagement opened.
///
/// Applied to the *derived* id and to an inherited override alike. An override
/// exists so a delegated child shares its parent's window; namespacing it
/// keeps that working within one engagement (parent and child derive the same
/// namespaced id) while making it impossible across two (a session id handed
/// over from engagement A resolves to a different id under B).
///
/// Unbound executions are untouched — an execution with no engagement has no
/// engagement to be partitioned by.
pub fn engagement_browser_session_id(session_id: &str, scope: &RetrievalScope) -> String {
    let Some((binding_kind, binding_id)) = scope_binding(scope) else {
        return session_id.to_string();
    };
    format!(
        "{BROWSER_SESSION_SCOPE_PREFIX}-{binding_kind}-{}-{session_id}",
        sanitize_engagement_segment(binding_id)
    )
}

/// Reduce an engagement id to characters an agent-browser session id and a
/// profile directory both tolerate. Distinctness is preserved by keeping every
/// character that survives and replacing the rest one-for-one, so two ids
/// cannot collapse into one namespace unless they differed only in separators.
fn sanitize_engagement_segment(engagement_id: &str) -> String {
    engagement_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn carrier(engagement_id: &str) -> EngagementAuthorityRef {
        EngagementAuthorityRef {
            engagement_id: engagement_id.to_string(),
            authority_revision: 1,
        }
    }

    /// Pins that the carrier is what decides containment. If this ever read a
    /// goal label or a tool argument instead, an agent could name the scope it
    /// wanted to retrieve under.
    #[test]
    fn scope_comes_from_the_carrier() {
        assert_eq!(
            retrieval_scope_from_authority(None),
            Ok(RetrievalScope::Unbound)
        );
        assert_eq!(
            retrieval_scope_from_authority(Some(&carrier("eng-a"))),
            Ok(RetrievalScope::Bound {
                engagement_id: "eng-a".to_string()
            })
        );
    }

    /// Pins that a carrier with a blank id is an error, not an unbound read.
    /// Downgrading it would turn a broken binding into full retrieval, which
    /// is the loudest possible version of the vacuous-truth bug.
    #[test]
    fn blank_carrier_id_is_an_error_not_an_unbound_read() {
        let denial = retrieval_scope_from_authority(Some(&carrier("   ")))
            .expect_err("a blank engagement id must not resolve to a scope");
        assert!(
            denial.contains("blank engagement id"),
            "denial should say what was blank, got: {denial}"
        );
    }

    /// Pins the runtime-arg reader against the three shapes that are not a
    /// usable engagement id. Absent is unbound (the surface carries none);
    /// wrong-typed and blank are errors, because a surface that meant to bind
    /// and failed must not retrieve everything.
    #[test]
    fn runtime_arg_reader_separates_absent_from_unreadable() {
        assert_eq!(
            retrieval_scope_from_runtime_args(&json!({ "query": "x" })),
            Ok(RetrievalScope::Unbound)
        );
        assert_eq!(
            retrieval_scope_from_runtime_args(&json!({ "__engagement_id": null })),
            Ok(RetrievalScope::Unbound)
        );
        assert_eq!(
            retrieval_scope_from_runtime_args(&json!({ "__engagement_id": "eng-a" })),
            Ok(RetrievalScope::Bound {
                engagement_id: "eng-a".to_string()
            })
        );
        assert!(retrieval_scope_from_runtime_args(&json!({ "__engagement_id": 12 })).is_err());
        assert!(retrieval_scope_from_runtime_args(&json!({ "__engagement_id": "  " })).is_err());
    }

    /// Pins that the unlabelled-corpus gate is silent for unbound executions
    /// and refuses for bound ones. A gate that refused unbound reads would
    /// break every ordinary task that greps a file.
    #[test]
    fn unlabelled_corpus_refused_only_under_a_bound_execution() {
        assert_eq!(
            cross_engagement_corpus_refusal("search_notes", &RetrievalScope::Unbound),
            None
        );
        let bound = RetrievalScope::bound("eng-a").expect("non-empty id binds");
        let refusal = cross_engagement_corpus_refusal("search_notes", &bound)
            .expect("a bound execution must not read the owner's notes");
        assert!(refusal.starts_with("NOT RETRIEVED"));
        assert!(refusal.contains("engagement `eng-a`"));
        assert!(refusal.contains("the owner's notes"));

        // A meeting is the narrower binding, so it must not be the looser
        // gate: a room that could read the owner's notes would undo the whole
        // boundary while looking confined.
        let room = RetrievalScope::for_meeting("meet-1").expect("non-empty id binds");
        let room_refusal = cross_engagement_corpus_refusal("search_notes", &room)
            .expect("a meeting-confined execution must not read the owner's notes");
        assert!(room_refusal.contains("meeting `meet-1`"));
    }

    /// Pins that the gate matches the capability name case-insensitively and
    /// does not refuse the outward acts an engagement exists to perform.
    #[test]
    fn gate_covers_named_corpora_and_leaves_outward_acts_alone() {
        let bound = RetrievalScope::bound("eng-a").expect("non-empty id binds");
        assert!(cross_engagement_corpus_refusal("GREP", &bound).is_some());
        assert!(cross_engagement_corpus_refusal("agentmail-send", &bound).is_none());
        assert!(cross_engagement_corpus_refusal("browser", &bound).is_none());
        assert!(cross_engagement_corpus_refusal("search_memory", &bound).is_none());
        assert_eq!(unlabelled_corpus_capabilities().len(), 17);
    }

    /// Pins the two facts that make [`ENGAGEMENT_LABELLED_ONLY_CAPABILITIES`] a
    /// separate list rather than more rows on the unlabelled one.
    ///
    /// First, these capabilities are NOT refused under a bound engagement:
    /// memory is filtered there, not withheld, and refusing it would delete the
    /// engagement's own recall along with everyone else's. Second, no name may
    /// sit on both lists — a capability on both would be refused for two
    /// contradictory reasons, and whichever list a caller consulted first would
    /// decide, which is how a corpus that IS filterable comes to be reported as
    /// carrying no label at all.
    #[test]
    fn the_engagement_labelled_only_list_is_disjoint_and_survives_an_engagement() {
        let bound = RetrievalScope::bound("eng-a").expect("non-empty id binds");
        let unlabelled = unlabelled_corpus_capabilities();

        assert_eq!(
            engagement_labelled_only_capabilities(),
            vec!["search_memory", "forget_memory"],
            "the list must match the capability names the compiled handlers are \
             registered under; a spelling that drifts refuses nothing and reports success"
        );

        for capability in engagement_labelled_only_capabilities() {
            assert!(
                !unlabelled.contains(&capability),
                "`{capability}` cannot be both filterable-by-engagement and unlabelled"
            );
            assert!(
                cross_engagement_corpus_refusal(capability, &bound).is_none(),
                "`{capability}` must still reach memory under an engagement: the runtime \
                 stamps `__engagement_id` and the records carry a matching label, so it \
                 is filtered rather than withheld"
            );
            assert_eq!(
                engagement_labelled_only_corpus(capability),
                Some("the owner's memory"),
                "a refusal must be able to name the corpus, not only the tool"
            );
        }

        assert_eq!(
            engagement_labelled_only_corpus("search_notes"),
            None,
            "a capability off this list must not borrow its refusal"
        );
    }

    /// Pins that an unreadable carrier yields NO scope, so a render site
    /// renders nothing. The tempting fallback — a `Bound` scope with a
    /// nonsense id — would still admit every `neutral` item, which is the
    /// vacuous-truth bug wearing a fail-closed costume.
    #[test]
    fn unreadable_carrier_yields_no_scope_at_all() {
        assert_eq!(contained_retrieval_scope(Some(&carrier("   "))), None);
        assert_eq!(
            contained_retrieval_scope(None),
            Some(RetrievalScope::Unbound),
            "no carrier is the unbound path and must stay unnarrowed"
        );
        assert_eq!(
            contained_retrieval_scope(Some(&carrier("eng-a"))),
            Some(RetrievalScope::Bound {
                engagement_id: "eng-a".to_string()
            })
        );
    }

    /// Pins the browser partition: same engagement shares a window, different
    /// engagements cannot, and an unbound execution is untouched. Without the
    /// partition a research visit for one counterparty reuses the browser
    /// state left behind by another.
    #[test]
    fn browser_sessions_partition_by_engagement() {
        let a = RetrievalScope::bound("eng-a").expect("non-empty id binds");
        let b = RetrievalScope::bound("eng-b").expect("non-empty id binds");
        assert_eq!(
            engagement_browser_session_id("magician-exec-1", &RetrievalScope::Unbound),
            "magician-exec-1"
        );
        assert_eq!(
            engagement_browser_session_id("magician-exec-1", &a),
            "scope-engagement-eng-a-magician-exec-1"
        );
        // A meeting confines the browser too, and into a different namespace
        // than the engagement it belongs to: two meetings with one
        // counterparty are one relationship and two rooms.
        let room = RetrievalScope::for_meeting("eng-a").expect("non-empty id binds");
        assert_ne!(
            engagement_browser_session_id("magician-exec-1", &room),
            engagement_browser_session_id("magician-exec-1", &a),
        );
        assert_eq!(
            engagement_browser_session_id("magician-exec-1", &a),
            engagement_browser_session_id("magician-exec-1", &a),
        );
        assert_ne!(
            engagement_browser_session_id("magician-exec-1", &a),
            engagement_browser_session_id("magician-exec-1", &b),
        );
    }

    /// Pins that two engagement ids differing only in characters the
    /// sanitizer rewrites still land in different namespaces when the
    /// difference survives, and that the sanitizer never emits a character
    /// outside the session-id alphabet.
    #[test]
    fn engagement_segment_sanitizer_keeps_ids_distinct() {
        assert_eq!(sanitize_engagement_segment("eng/A_1"), "eng-a-1");
        assert_eq!(sanitize_engagement_segment("eng/B_1"), "eng-b-1");
        assert_ne!(
            sanitize_engagement_segment("eng/A_1"),
            sanitize_engagement_segment("eng/B_1")
        );
        assert!(sanitize_engagement_segment("eng:a b/c")
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-'));
    }
}
