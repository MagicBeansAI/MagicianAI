//! Stage 4: continuation, dispatch reconciliation, thread lifecycle, and
//! cross-engine envelope helpers for the dormant Codex adapter.
//!
//! These decisions are provider-free. The live adapter consults them; HTTP
//! handlers still do not select Codex.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    selection::{
        engine_str, CodingBillingBasis, CodingContinuationRef, CodingDispatchState,
        CodingEngineUsage,
    },
    CodingEngineKind,
};

const ENVELOPE_DOMAIN: &str = "magician.coding_engine.cross_engine_envelope.v1";
const OUTBOX_DOMAIN: &str = "magician.coding_engine.thread_outbox.v1";
const MAX_ENVELOPE_REFS: usize = 16;
pub const MAX_CODEX_FOLLOW_UPS: usize = 8;
/// Grok has no Pi-style internal queue. Follow-up/steer share this cap.
pub const MAX_GROK_FOLLOW_UPS: usize = MAX_CODEX_FOLLOW_UPS;
/// Claude `-p` is one-shot; print mode has no mid-turn RPC. Kept next to the
/// Codex/Grok caps so a later FIFO does not invent a different bound.
#[allow(dead_code)]
pub const MAX_CLAUDE_FOLLOW_UPS: usize = MAX_CODEX_FOLLOW_UPS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationFreshReason {
    None,
    ContinuationLost,
    CrossEngine,
    ScopeMismatch,
    StaleGeneration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinuationResume {
    Resume { thread_id: String, generation: u64 },
    Fresh { reason: ContinuationFreshReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryReadError {
    Truncated,
    Malformed,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryTurn {
    pub turn_id: String,
    pub parent_turn_id: Option<String>,
    pub input_digest: Option<String>,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchReconcile {
    RetryPrepared,
    Adopt { turn_id: String },
    ContinuationLost,
    DispatchUnknown { reason: &'static str },
}

/// A predecessor thread that is gone *before* any new turn write is lost,
/// not unknown. After `RequestMayHaveStarted`, use [`reconcile_dispatch`].
pub fn classify_pre_dispatch_thread_read(
    read: Result<(), HistoryReadError>,
) -> ContinuationFreshReason {
    match read {
        Ok(()) => ContinuationFreshReason::None,
        Err(HistoryReadError::Unavailable) => ContinuationFreshReason::ContinuationLost,
        Err(_) => ContinuationFreshReason::ContinuationLost,
    }
}

pub fn resume_or_fresh(
    previous: Option<&CodingContinuationRef>,
    requested: CodingEngineKind,
    scope_root: &Path,
    project_root: &Path,
    root_task_id: Option<&str>,
    generation: u64,
) -> ContinuationResume {
    let Some(previous) = previous else {
        return ContinuationResume::Fresh {
            reason: ContinuationFreshReason::None,
        };
    };
    if previous.engine != requested {
        return ContinuationResume::Fresh {
            reason: ContinuationFreshReason::CrossEngine,
        };
    }
    let expected = match requested {
        CodingEngineKind::Pi => CodingContinuationRef::for_pi_session(
            previous.native_session_id.clone(),
            scope_root,
            project_root,
            root_task_id,
        ),
        CodingEngineKind::CodexAppServer => CodingContinuationRef::for_codex_thread(
            previous.native_session_id.clone(),
            scope_root,
            project_root,
            root_task_id,
        ),
        CodingEngineKind::GrokAcp => CodingContinuationRef::for_grok_session(
            previous.native_session_id.clone(),
            scope_root,
            project_root,
            root_task_id,
        ),
        CodingEngineKind::ClaudeCode => CodingContinuationRef::for_claude_session(
            previous.native_session_id.clone(),
            scope_root,
            project_root,
            root_task_id,
        ),
        CodingEngineKind::AgyCli => CodingContinuationRef::for_agy_session(
            previous.native_session_id.clone(),
            scope_root,
            project_root,
            root_task_id,
        ),
    };
    if expected.scope_binding_digest != previous.scope_binding_digest
        || expected.project_binding_digest != previous.project_binding_digest
        || expected.root_task_id != previous.root_task_id
    {
        return ContinuationResume::Fresh {
            reason: ContinuationFreshReason::ScopeMismatch,
        };
    }
    if previous.generation > generation {
        return ContinuationResume::Fresh {
            reason: ContinuationFreshReason::StaleGeneration,
        };
    }
    ContinuationResume::Resume {
        thread_id: previous.native_session_id.clone(),
        generation: previous.generation.saturating_add(1),
    }
}

/// Decide, from a persisted dispatch state plus Codex's own turn history,
/// whether a request that may have started actually did.
///
/// # SUPERSEDED — nothing calls this, and nothing should start
///
/// **Status as of 2026-08-29: not wired, and deliberately not wired.** Every
/// call site is in this file's own `mod tests`. It is described here rather than
/// deleted because it reads as finished work — it is complete, it is well
/// tested, and a reader who meets `RequestMayHaveStarted` in a live ledger will
/// find it and reasonably conclude it is the consumer. It is not.
///
/// ## What replaced it
///
/// The question — *did this effect fire?* — is answered generically by the
/// loop's effect ledger, `execution::agentic::run_loop::effects::EffectLedger`:
/// an intent committed before the fire (`driver_worker::record_batch_intents`),
/// an outcome after it, and `EffectLedger::disposition` on recovery. A coding
/// job declares `RetrySafety::Reattachable`
/// (`run_loop::phases::apply::declared_retry_safety`), so its disposition is
/// `EffectDisposition::Reattach` carrying a **coding invocation id**, which
/// `WorkerHost::reattach_state` resolves through
/// `ledger::invocation_continuation` to the session the job reported. No
/// session, no guess: the driver holds it as `EffectIndeterminate`.
///
/// ## Why the generic mechanism is not weaker here
///
/// The case that argues for provider-history matching is *the request reached
/// the provider but the intent write was lost*. The effect ledger survives that
/// one: the reattach ref is not read out of the lost row, it is **derived** —
/// `ledger::coding_invocation_id_for_effect(execution_id, effect_id)` is a
/// blake3 of two values the pickup already holds, and `disposition` falls back
/// to the re-minted ref when the row is absent. The key survives the loss of the
/// row, and the coding ledger on disk is the authority it opens.
///
/// ## What it would still take to wire this, if that ever changes
///
/// Three pieces, none of which exists:
///
/// 1. **A producer for [`HistoryTurn`].** The type has none anywhere in the
///    workspace — no constructor outside this file's tests. The transport is
///    closer than that sounds and still not there: `codex::thread/read` IS
///    allowlisted and IS called on the resume path, but its response is
///    discarded on the spot, and `thread/list` is allowlisted and never called.
///    So wiring this is not a call-site change; it is writing the parser from
///    that discarded payload into this shape first — and whether the payload
///    even carries per-turn parent ids and input digests is unverified.
/// 2. **A writer for `base_turn_id`.** The matching below keys on
///    `turn.parent_turn_id == base`, and `base` is always `None` in production
///    (see `CodingDispatchState::Prepared::base_turn_id`). With `None`, a
///    resumed thread's new turn — whose parent is the previous turn — matches
///    nothing, so this returns `no_history_match` in exactly the situation it
///    exists to resolve.
/// 3. **A shared input digest.** `expected_input_digest` is Magician's own
///    `ledger::coding_input_digest`. Nothing sends it to Codex and nothing reads
///    a digest back, so `HistoryTurn::input_digest` has no source that could
///    equal it.
///
/// ## The sliver that is genuinely uncovered
///
/// A worker that dies after `ledger::mark_invocation_may_have_started` and
/// before the engine names a session leaves `RequestMayHaveStarted` with no
/// continuation, and recovery holds indeterminate. Provider history could only
/// speak to that for a **resumed** thread, since a fresh thread's id is minted
/// inside the turn and there is nothing to query — and even then it needs all
/// three pieces above. `ledger::attach_live_invocation_session` already narrowed
/// that window from *the whole turn* to *the engine handshake*.
///
/// Recovery is also loop-only: `run_coding_task` is reached from chat, from
/// VibeDev and from direct handler calls as well as from `Apply`, and only
/// `Apply` has an effect row. Those paths have no resume driver either, so they
/// are uncovered by both mechanisms rather than by one.
pub fn reconcile_dispatch(
    dispatch: &CodingDispatchState,
    history: Result<&[HistoryTurn], HistoryReadError>,
    expected_input_digest: &str,
) -> DispatchReconcile {
    if dispatch.automatic_retry_allowed() {
        return DispatchReconcile::RetryPrepared;
    }
    if matches!(dispatch, CodingDispatchState::Settled { .. }) {
        return DispatchReconcile::DispatchUnknown {
            reason: "already_settled",
        };
    }
    let turns = match history {
        Ok(turns) => turns,
        Err(HistoryReadError::Unavailable) => {
            return DispatchReconcile::DispatchUnknown {
                reason: "unavailable_history",
            };
        },
        Err(HistoryReadError::Truncated) => {
            return DispatchReconcile::DispatchUnknown {
                reason: "truncated_history",
            };
        },
        Err(HistoryReadError::Malformed) => {
            return DispatchReconcile::DispatchUnknown {
                reason: "malformed_history",
            };
        },
    };
    if turns.iter().any(|turn| !turn.complete) {
        return DispatchReconcile::DispatchUnknown {
            reason: "incomplete_history",
        };
    }
    let base = match dispatch {
        CodingDispatchState::Prepared { base_turn_id, .. }
        | CodingDispatchState::RequestMayHaveStarted { base_turn_id, .. } => {
            base_turn_id.as_deref()
        },
        _ => None,
    };
    let matches: Vec<&HistoryTurn> = turns
        .iter()
        .filter(|turn| {
            turn.parent_turn_id.as_deref() == base
                && turn.input_digest.as_deref() == Some(expected_input_digest)
        })
        .collect();
    match matches.as_slice() {
        [only] => DispatchReconcile::Adopt {
            turn_id: only.turn_id.clone(),
        },
        [] => DispatchReconcile::DispatchUnknown {
            reason: "no_history_match",
        },
        _ => DispatchReconcile::DispatchUnknown {
            reason: "ambiguous_history",
        },
    }
}

pub fn fork_params(continuation: &CodingContinuationRef) -> Result<Value, &'static str> {
    if continuation.engine != CodingEngineKind::CodexAppServer {
        return Err("checkpoint fork is Codex-only");
    }
    let Some(turn_id) = continuation.last_completed_turn_id.as_deref() else {
        return Err("checkpoint fork requires a recorded turn id");
    };
    if turn_id.is_empty() || continuation.native_session_id.is_empty() {
        return Err("checkpoint fork is missing a native thread or turn");
    }
    Ok(json!({
        "threadId": continuation.native_session_id,
        "turnId": turn_id,
    }))
}

pub fn cross_engine_envelope_digest(
    root_task_id: &str,
    proposal_ids: &[&str],
    artifact_ids: &[&str],
) -> Result<String, &'static str> {
    if proposal_ids.len().saturating_add(artifact_ids.len()) > MAX_ENVELOPE_REFS {
        return Err("cross-engine envelope exceeds the ref bound");
    }
    if proposal_ids.iter().any(|id| id.is_empty()) || artifact_ids.iter().any(|id| id.is_empty()) {
        return Err("cross-engine envelope refs must be non-empty");
    }
    let mut hasher = blake3::Hasher::new();
    absorb(&mut hasher, "domain", ENVELOPE_DOMAIN);
    absorb(&mut hasher, "root_task_id", root_task_id);
    for id in proposal_ids {
        absorb(&mut hasher, "proposal", id);
    }
    for id in artifact_ids {
        absorb(&mut hasher, "artifact", id);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub fn verification_engine_label(engine: CodingEngineKind) -> &'static str {
    engine_str(engine)
}

pub fn cost_usd_for_display(usage: &CodingEngineUsage) -> Option<&str> {
    usage.cost_usd.as_deref().filter(|cost| !cost.is_empty())
}

pub fn dollar_ceiling_permitted(usage: &CodingEngineUsage, require_dollar: bool) -> bool {
    if !require_dollar {
        return true;
    }
    matches!(
        usage.billing_basis,
        CodingBillingBasis::ProviderPriced | CodingBillingBasis::External
    ) && cost_usd_for_display(usage).is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadLifecycleOp {
    Archive,
    Unarchive,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadLifecycleOutcome {
    Success,
    NotFound,
    Unavailable,
    Retry,
    Ambiguous,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThreadLifecycleRecord {
    pub op: ThreadLifecycleOp,
    pub thread_digest: String,
    pub outcome: ThreadLifecycleOutcome,
}

pub fn thread_outbox_dir(scope_root: &Path) -> PathBuf {
    scope_root.join("coding_engine").join("thread_outbox")
}

pub fn thread_digest(thread_id: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    absorb(&mut hasher, "domain", OUTBOX_DOMAIN);
    absorb(&mut hasher, "thread_id", thread_id);
    hasher.finalize().to_hex().to_string()
}

pub fn store_thread_outbox(
    scope_root: &Path,
    thread_id: &str,
    op: ThreadLifecycleOp,
    outcome: ThreadLifecycleOutcome,
) -> std::io::Result<ThreadLifecycleRecord> {
    let record = ThreadLifecycleRecord {
        op,
        thread_digest: thread_digest(thread_id),
        outcome,
    };
    let dir = thread_outbox_dir(scope_root);
    std::fs::create_dir_all(&dir)?;
    let name = format!("{}-{}.json", record.thread_digest, lifecycle_op_name(op));
    let path = dir.join(name);
    let bytes = serde_json::to_vec_pretty(&record)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    std::fs::write(path, bytes)?;
    Ok(record)
}

pub fn reconcile_lifecycle_receipt(
    previous: Option<ThreadLifecycleOutcome>,
    provider: Result<ThreadLifecycleOutcome, HistoryReadError>,
    scope_revoked: bool,
) -> ThreadLifecycleOutcome {
    if scope_revoked {
        return ThreadLifecycleOutcome::Revoked;
    }
    match provider {
        Ok(outcome) => outcome,
        Err(HistoryReadError::Unavailable) => ThreadLifecycleOutcome::Unavailable,
        Err(HistoryReadError::Truncated | HistoryReadError::Malformed) => {
            if previous == Some(ThreadLifecycleOutcome::Success) {
                ThreadLifecycleOutcome::Ambiguous
            } else {
                ThreadLifecycleOutcome::Retry
            }
        },
    }
}

fn lifecycle_op_name(op: ThreadLifecycleOp) -> &'static str {
    match op {
        ThreadLifecycleOp::Archive => "archive",
        ThreadLifecycleOp::Unarchive => "unarchive",
        ThreadLifecycleOp::Delete => "delete",
    }
}

fn absorb(hasher: &mut blake3::Hasher, label: &str, value: &str) {
    hasher.update(&(label.len() as u64).to_le_bytes());
    hasher.update(label.as_bytes());
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::path::Path;

    use super::*;

    fn continuation(engine: CodingEngineKind, session: &str) -> CodingContinuationRef {
        match engine {
            CodingEngineKind::Pi => CodingContinuationRef::for_pi_session(
                session,
                Path::new("/tmp/scope"),
                Path::new("/tmp/project"),
                Some("root"),
            ),
            CodingEngineKind::CodexAppServer => CodingContinuationRef::for_codex_thread(
                session,
                Path::new("/tmp/scope"),
                Path::new("/tmp/project"),
                Some("root"),
            ),
            CodingEngineKind::GrokAcp => CodingContinuationRef::for_grok_session(
                session,
                Path::new("/tmp/scope"),
                Path::new("/tmp/project"),
                Some("root"),
            ),
            CodingEngineKind::ClaudeCode => CodingContinuationRef::for_claude_session(
                session,
                Path::new("/tmp/scope"),
                Path::new("/tmp/project"),
                Some("root"),
            ),
            CodingEngineKind::AgyCli => CodingContinuationRef::for_agy_session(
                session,
                Path::new("/tmp/scope"),
                Path::new("/tmp/project"),
                Some("root"),
            ),
        }
    }

    #[test]
    fn same_engine_predecessor_resumes_and_cross_engine_starts_fresh() {
        let pi = continuation(CodingEngineKind::Pi, "sess-pi");
        let codex = continuation(CodingEngineKind::CodexAppServer, "thread-1");
        assert!(matches!(
            resume_or_fresh(
                Some(&codex),
                CodingEngineKind::CodexAppServer,
                Path::new("/tmp/scope"),
                Path::new("/tmp/project"),
                Some("root"),
                0,
            ),
            ContinuationResume::Resume { thread_id, .. } if thread_id == "thread-1"
        ));
        assert!(matches!(
            resume_or_fresh(
                Some(&pi),
                CodingEngineKind::CodexAppServer,
                Path::new("/tmp/scope"),
                Path::new("/tmp/project"),
                Some("root"),
                0,
            ),
            ContinuationResume::Fresh {
                reason: ContinuationFreshReason::CrossEngine
            }
        ));
        assert!(matches!(
            resume_or_fresh(
                Some(&codex),
                CodingEngineKind::Pi,
                Path::new("/tmp/scope"),
                Path::new("/tmp/project"),
                Some("root"),
                0,
            ),
            ContinuationResume::Fresh {
                reason: ContinuationFreshReason::CrossEngine
            }
        ));
        let grok = continuation(CodingEngineKind::GrokAcp, "sess-grok");
        assert!(matches!(
            resume_or_fresh(
                Some(&grok),
                CodingEngineKind::GrokAcp,
                Path::new("/tmp/scope"),
                Path::new("/tmp/project"),
                Some("root"),
                0,
            ),
            ContinuationResume::Resume { thread_id, .. } if thread_id == "sess-grok"
        ));
        assert!(matches!(
            resume_or_fresh(
                Some(&pi),
                CodingEngineKind::GrokAcp,
                Path::new("/tmp/scope"),
                Path::new("/tmp/project"),
                Some("root"),
                0,
            ),
            ContinuationResume::Fresh {
                reason: ContinuationFreshReason::CrossEngine
            }
        ));
        assert!(matches!(
            resume_or_fresh(
                Some(&grok),
                CodingEngineKind::Pi,
                Path::new("/tmp/scope"),
                Path::new("/tmp/project"),
                Some("root"),
                0,
            ),
            ContinuationResume::Fresh {
                reason: ContinuationFreshReason::CrossEngine
            }
        ));
        assert!(matches!(
            resume_or_fresh(
                Some(&grok),
                CodingEngineKind::CodexAppServer,
                Path::new("/tmp/scope"),
                Path::new("/tmp/project"),
                Some("root"),
                0,
            ),
            ContinuationResume::Fresh {
                reason: ContinuationFreshReason::CrossEngine
            }
        ));
        let claude = continuation(CodingEngineKind::ClaudeCode, "sess-claude");
        let agy = continuation(CodingEngineKind::AgyCli, "sess-agy");
        assert!(matches!(
            resume_or_fresh(
                Some(&claude),
                CodingEngineKind::ClaudeCode,
                Path::new("/tmp/scope"),
                Path::new("/tmp/project"),
                Some("root"),
                0,
            ),
            ContinuationResume::Resume { thread_id, .. } if thread_id == "sess-claude"
        ));
        assert!(matches!(
            resume_or_fresh(
                Some(&grok),
                CodingEngineKind::ClaudeCode,
                Path::new("/tmp/scope"),
                Path::new("/tmp/project"),
                Some("root"),
                0,
            ),
            ContinuationResume::Fresh {
                reason: ContinuationFreshReason::CrossEngine
            }
        ));
        assert!(matches!(
            resume_or_fresh(
                Some(&pi),
                CodingEngineKind::ClaudeCode,
                Path::new("/tmp/scope"),
                Path::new("/tmp/project"),
                Some("root"),
                0,
            ),
            ContinuationResume::Fresh {
                reason: ContinuationFreshReason::CrossEngine
            }
        ));
        assert!(matches!(
            resume_or_fresh(
                Some(&codex),
                CodingEngineKind::AgyCli,
                Path::new("/tmp/scope"),
                Path::new("/tmp/project"),
                Some("root"),
                0,
            ),
            ContinuationResume::Fresh {
                reason: ContinuationFreshReason::CrossEngine
            }
        ));
        assert!(matches!(
            resume_or_fresh(
                Some(&agy),
                CodingEngineKind::ClaudeCode,
                Path::new("/tmp/scope"),
                Path::new("/tmp/project"),
                Some("root"),
                0,
            ),
            ContinuationResume::Fresh {
                reason: ContinuationFreshReason::CrossEngine
            }
        ));
    }

    #[test]
    fn stale_and_cross_scope_continuations_are_rejected() {
        let current = continuation(CodingEngineKind::CodexAppServer, "thread-1");
        assert!(matches!(
            resume_or_fresh(
                Some(&current),
                CodingEngineKind::CodexAppServer,
                Path::new("/tmp/other-scope"),
                Path::new("/tmp/project"),
                Some("root"),
                0,
            ),
            ContinuationResume::Fresh {
                reason: ContinuationFreshReason::ScopeMismatch
            }
        ));
        let mut stale = current.clone();
        stale.generation = 9;
        assert!(matches!(
            resume_or_fresh(
                Some(&stale),
                CodingEngineKind::CodexAppServer,
                Path::new("/tmp/scope"),
                Path::new("/tmp/project"),
                Some("root"),
                1,
            ),
            ContinuationResume::Fresh {
                reason: ContinuationFreshReason::StaleGeneration
            }
        ));
    }

    #[test]
    fn pre_dispatch_thread_loss_is_continuation_lost() {
        assert_eq!(
            classify_pre_dispatch_thread_read(Err(HistoryReadError::Unavailable)),
            ContinuationFreshReason::ContinuationLost
        );
        assert_eq!(
            classify_pre_dispatch_thread_read(Ok(())),
            ContinuationFreshReason::None
        );
    }

    #[test]
    fn prepared_retries_and_started_without_a_match_stays_unknown() {
        let prepared = CodingDispatchState::Prepared {
            canonical_input_digest: "in".into(),
            base_turn_id: Some("t0".into()),
        };
        assert_eq!(
            reconcile_dispatch(&prepared, Ok(&[]), "in"),
            DispatchReconcile::RetryPrepared
        );
        let started = prepared
            .clone()
            .mark_request_may_have_started()
            .expect("advance");
        assert_eq!(
            reconcile_dispatch(&started, Ok(&[]), "in"),
            DispatchReconcile::DispatchUnknown {
                reason: "no_history_match"
            }
        );
        assert_eq!(
            reconcile_dispatch(&started, Err(HistoryReadError::Truncated), "in"),
            DispatchReconcile::DispatchUnknown {
                reason: "truncated_history"
            }
        );
        assert_eq!(
            reconcile_dispatch(&started, Err(HistoryReadError::Unavailable), "in"),
            DispatchReconcile::DispatchUnknown {
                reason: "unavailable_history"
            }
        );
    }

    #[test]
    fn one_matching_successor_is_adopted_and_two_are_ambiguous() {
        let started = CodingDispatchState::RequestMayHaveStarted {
            canonical_input_digest: "in".into(),
            base_turn_id: Some("t0".into()),
        };
        let one = [HistoryTurn {
            turn_id: "t1".into(),
            parent_turn_id: Some("t0".into()),
            input_digest: Some("in".into()),
            complete: true,
        }];
        assert_eq!(
            reconcile_dispatch(&started, Ok(&one), "in"),
            DispatchReconcile::Adopt {
                turn_id: "t1".into()
            }
        );
        let two = [
            one[0].clone(),
            HistoryTurn {
                turn_id: "t2".into(),
                parent_turn_id: Some("t0".into()),
                input_digest: Some("in".into()),
                complete: true,
            },
        ];
        assert_eq!(
            reconcile_dispatch(&started, Ok(&two), "in"),
            DispatchReconcile::DispatchUnknown {
                reason: "ambiguous_history"
            }
        );
        let incomplete = [HistoryTurn {
            turn_id: "t1".into(),
            parent_turn_id: Some("t0".into()),
            input_digest: Some("in".into()),
            complete: false,
        }];
        assert_eq!(
            reconcile_dispatch(&started, Ok(&incomplete), "in"),
            DispatchReconcile::DispatchUnknown {
                reason: "incomplete_history"
            }
        );
    }

    #[test]
    fn fork_requires_a_recorded_codex_turn() {
        let mut recorded = continuation(CodingEngineKind::CodexAppServer, "thread-1");
        assert!(fork_params(&recorded).is_err());
        recorded.last_completed_turn_id = Some("turn-9".into());
        let params = fork_params(&recorded).expect("fork");
        assert_eq!(params["threadId"], "thread-1");
        assert_eq!(params["turnId"], "turn-9");
        assert!(fork_params(&continuation(CodingEngineKind::Pi, "sess")).is_err());
        assert!(fork_params(&continuation(CodingEngineKind::GrokAcp, "sess-grok")).is_err());
    }

    #[test]
    fn envelope_is_bounded_and_transcript_free() {
        let digest = cross_engine_envelope_digest("root", &["prop-1"], &["art-1"]).expect("ok");
        assert_eq!(digest.len(), 64);
        assert!(cross_engine_envelope_digest("root", &["p"; 16], &["a"]).is_err());
        assert!(cross_engine_envelope_digest("root", &[""], &[]).is_err());
    }

    #[test]
    fn unknown_entitlement_is_never_a_dollar_zero() {
        let usage = CodingEngineUsage {
            input_tokens: 4,
            output_tokens: 2,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd: None,
            billing_basis: CodingBillingBasis::ChatgptEntitlement,
        };
        assert_eq!(cost_usd_for_display(&usage), None);
        assert!(!dollar_ceiling_permitted(&usage, true));
        assert!(dollar_ceiling_permitted(&usage, false));
    }

    #[test]
    fn thread_outbox_lives_outside_task_directories() {
        let scope = tempfile::tempdir().expect("scope");
        let record = store_thread_outbox(
            scope.path(),
            "thread-secret",
            ThreadLifecycleOp::Delete,
            ThreadLifecycleOutcome::Success,
        )
        .expect("store");
        assert!(!record.thread_digest.contains("thread-secret"));
        let dir = thread_outbox_dir(scope.path());
        assert!(dir.starts_with(scope.path().join("coding_engine")));
        assert!(!dir.to_string_lossy().contains("tasks"));
        let listed = std::fs::read_dir(&dir).expect("list");
        for entry in listed {
            let name = entry.expect("entry").file_name();
            assert!(
                !name.to_string_lossy().contains("thread-secret"),
                "{name:?}"
            );
        }
        assert_eq!(
            reconcile_lifecycle_receipt(None, Ok(ThreadLifecycleOutcome::NotFound), false),
            ThreadLifecycleOutcome::NotFound
        );
        assert_eq!(
            reconcile_lifecycle_receipt(
                Some(ThreadLifecycleOutcome::Success),
                Err(HistoryReadError::Malformed),
                false
            ),
            ThreadLifecycleOutcome::Ambiguous
        );
        assert_eq!(
            reconcile_lifecycle_receipt(None, Ok(ThreadLifecycleOutcome::Success), true),
            ThreadLifecycleOutcome::Revoked
        );
    }

    #[test]
    fn verification_uses_the_shared_engine_label() {
        assert_eq!(verification_engine_label(CodingEngineKind::Pi), "pi");
        assert_eq!(
            verification_engine_label(CodingEngineKind::CodexAppServer),
            "codex_app_server"
        );
        assert_eq!(
            verification_engine_label(CodingEngineKind::GrokAcp),
            "grok_acp"
        );
        assert_eq!(
            verification_engine_label(CodingEngineKind::ClaudeCode),
            "claude_code"
        );
        assert_eq!(
            verification_engine_label(CodingEngineKind::AgyCli),
            "agy_cli"
        );
    }
}
