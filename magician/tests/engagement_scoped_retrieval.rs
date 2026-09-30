//! §5A.2 — engagement-scoped retrieval, proved against a corpus that really
//! contains the thing that must not come back.
//!
//! `docs/plans/2026-08-07-opc-engagements-contextual-authority.md` §5A.2.
//!
//! # How these tests are built so they cannot pass vacuously
//!
//! Every containment test here does the same two things in the same order:
//!
//! 1. retrieves **unbound** and asserts the other engagement's entry IS
//!    returned, and
//! 2. retrieves **bound** and asserts it is not.
//!
//! Step 1 is not decoration. A containment test that only asserts absence
//! passes just as well when the seeded corpus never loaded, when the tier name
//! was misspelled, when the query matched nothing, and when the filter is
//! deleted and there was nothing to find in the first place. Asserting the
//! entry is retrievable unbound is what makes the bound assertion mean
//! "filtered" instead of "empty" — and it is what makes deleting the filter
//! turn these tests red rather than green.

use std::collections::BTreeMap;

use magician::magician_v2::agents::memory_tiers::{
    MemoryTierDefinition, RenderConfig, RetentionMode, TierFieldSchema, TierScope,
};
use magician::magician_v2::agents::{
    load_memory_candidate_documents, storage::AgentStorage, AgentMemoryService,
    MemoryCandidateDocument, MemoryCandidateRequest, MemoryRenderRequest, RetrievalScope,
};
use magician::magician_v2::engagement_retrieval::{
    contained_retrieval_scope, cross_engagement_corpus_refusal, engagement_browser_session_id,
    retrieval_scope_from_runtime_args,
};
use magician::magician_v2::engagements::EngagementAuthorityRef;
use magician::magician_v2::work_context::WorkAuthorityRef;
use magician_vector_index::memory_record::V3MemoryTierRecord;
use serde_json::{json, Value};
use tempfile::TempDir;

const ENGAGEMENT_A: &str = "eng-northwind";
const ENGAGEMENT_B: &str = "eng-tidewater";
const AGENT: &str = "ambassador";
const TIER: &str = "counterparty_notes";
/// Every seeded entry carries this phrase so keyword ranking scores all of
/// them for one query. A retrieval that returns nothing because the query
/// missed would otherwise look exactly like a retrieval that was contained.
const SHARED_QUERY: &str = "diligence follow-up";

fn scoped_storage(tmp: &TempDir) -> AgentStorage {
    let root = tmp
        .path()
        .join("scopes")
        .join("owner")
        .join("default")
        .join("agent_runtime");
    std::fs::create_dir_all(&root).expect("create scoped memory root");
    AgentStorage::with_scoped_memory_root(root)
}

fn notes_tier() -> MemoryTierDefinition {
    let mut schema = BTreeMap::new();
    schema.insert(
        "notes".to_string(),
        TierFieldSchema::Collection {
            max_items: None,
            item_schema: None,
        },
    );
    MemoryTierDefinition {
        name: TIER.to_string(),
        scope: TierScope::Agent,
        description: "Notes taken while working one counterparty".to_string(),
        schema,
        render: RenderConfig {
            format: "list".to_string(),
            template: String::new(),
        },
        retention: RetentionMode::Forever,
    }
}

fn note(key: &str, engagement_scope: Option<&str>) -> Value {
    let mut item = json!({
        "key": key,
        "insight": format!("{SHARED_QUERY} recorded as {key}"),
    });
    if let Some(scope) = engagement_scope {
        item.as_object_mut()
            .expect("note is an object")
            .insert("engagement_scope".to_string(), json!(scope));
    }
    item
}

/// Seed one agent tier holding four notes: one per engagement, one explicitly
/// neutral, one unlabelled. Four rows because the rule has four answers and a
/// fixture with fewer cannot tell them apart.
async fn seed_notes(service: &AgentMemoryService, record_scope: Option<&str>) {
    let tier = notes_tier();
    let mut record = V3MemoryTierRecord::new(
        TIER,
        TierScope::Agent,
        None,
        Some("owner"),
        Some("default"),
        Some(AGENT),
    );
    record.fields.insert(
        "notes".to_string(),
        json!([
            note("ENTRY-A", Some(&format!("engagement:{ENGAGEMENT_A}"))),
            note("ENTRY-B", Some(&format!("engagement:{ENGAGEMENT_B}"))),
            note("ENTRY-NEUTRAL", Some("neutral")),
            note("ENTRY-UNLABELLED", None),
        ]),
    );
    if let Some(record_scope) = record_scope {
        record
            .fields
            .insert("engagement_scope".to_string(), json!(record_scope));
    }
    service
        .save_native_tier(AGENT, &tier, None, &record)
        .await
        .expect("seed counterparty notes tier");
}

async fn load(service: &AgentMemoryService, scope: RetrievalScope) -> Vec<MemoryCandidateDocument> {
    let tiers = [notes_tier()];
    load_memory_candidate_documents(
        service.storage(),
        AGENT,
        &tiers,
        &MemoryCandidateRequest {
            scope: TierScope::Agent,
            goal_id: None,
            recency_cutoff: None,
            include_environment_knowledge: true,
            retrieval_scope: scope,
        },
    )
    .await
    .expect("load candidates")
}

fn keys(candidates: &[MemoryCandidateDocument]) -> Vec<String> {
    let mut keys: Vec<String> = candidates
        .iter()
        .filter(|candidate| candidate.text.contains("ENTRY-"))
        .map(|candidate| {
            for marker in ["ENTRY-UNLABELLED", "ENTRY-NEUTRAL", "ENTRY-A", "ENTRY-B"] {
                if candidate.text.contains(marker) {
                    return marker.to_string();
                }
            }
            unreachable!("filtered to ENTRY- markers above")
        })
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// THE test this section exists for: an execution bound to engagement A must
/// not retrieve an entry written under engagement B.
///
/// The unbound assertion is the load-bearing half. Delete the filter in
/// `load_memory_candidate_documents` and the bound assertion fails; break the
/// fixture instead and the unbound assertion fails first, so a green run means
/// containment and never emptiness.
#[tokio::test]
async fn execution_bound_to_one_engagement_cannot_retrieve_anothers_entry() {
    let tmp = TempDir::new().expect("temp root");
    let service = AgentMemoryService::new(scoped_storage(&tmp));
    seed_notes(&service, None).await;

    let unbound = load(&service, RetrievalScope::Unbound).await;
    assert_eq!(
        keys(&unbound),
        vec![
            "ENTRY-A".to_string(),
            "ENTRY-B".to_string(),
            "ENTRY-NEUTRAL".to_string(),
            "ENTRY-UNLABELLED".to_string()
        ],
        "all four seeded notes must be retrievable unbound — otherwise the bound assertion below \
         proves nothing"
    );

    let bound = load(
        &service,
        RetrievalScope::bound(ENGAGEMENT_A).expect("engagement id binds"),
    )
    .await;
    assert_eq!(
        keys(&bound),
        vec!["ENTRY-A".to_string(), "ENTRY-NEUTRAL".to_string()],
        "an execution bound to {ENGAGEMENT_A} may retrieve its own entry and an explicitly \
         neutral one, and nothing else"
    );
}

/// Pins the vacuous-truth rule at the store level: an unlabelled entry is NOT
/// retrievable under a bound execution.
///
/// Separated from the test above because this is the assertion that decides
/// whether the whole boundary is real. Today's corpus is almost entirely
/// unlabelled, so a filter that admitted unlabelled entries would return
/// virtually the same rows as no filter at all while every "different
/// engagement" test still passed.
#[tokio::test]
async fn unlabelled_entries_are_not_retrievable_under_a_bound_execution() {
    let tmp = TempDir::new().expect("temp root");
    let service = AgentMemoryService::new(scoped_storage(&tmp));
    seed_notes(&service, None).await;

    let unbound = load(&service, RetrievalScope::Unbound).await;
    assert!(
        keys(&unbound).contains(&"ENTRY-UNLABELLED".to_string()),
        "the unlabelled entry must exist and be retrievable unbound"
    );

    let bound = load(
        &service,
        RetrievalScope::bound(ENGAGEMENT_A).expect("engagement id binds"),
    )
    .await;
    assert!(
        !keys(&bound).contains(&"ENTRY-UNLABELLED".to_string()),
        "unlabelled is not neutral: an entry nobody labelled must not be retrievable under an \
         engagement"
    );
    assert!(
        keys(&bound).contains(&"ENTRY-NEUTRAL".to_string()),
        "an entry explicitly labelled neutral must still cross, or the boundary would be a wall \
         rather than a filter"
    );
}

/// Pins container-to-item label inheritance: a tier record labelled to one
/// engagement carries its unlabelled rows with it, and never widens a row that
/// declared a different engagement.
///
/// Without inheritance an author would have to label every row of a
/// single-counterparty tier, and the rows they missed would be the ones that
/// leak.
#[tokio::test]
async fn a_record_level_label_reaches_its_unlabelled_rows() {
    let tmp = TempDir::new().expect("temp root");
    let service = AgentMemoryService::new(scoped_storage(&tmp));
    seed_notes(&service, Some(&format!("engagement:{ENGAGEMENT_B}"))).await;

    let unbound = load(&service, RetrievalScope::Unbound).await;
    assert_eq!(
        keys(&unbound).len(),
        4,
        "the record label must not change what exists, only who may read it"
    );

    let bound_b = load(
        &service,
        RetrievalScope::bound(ENGAGEMENT_B).expect("engagement id binds"),
    )
    .await;
    assert_eq!(
        keys(&bound_b),
        vec![
            "ENTRY-B".to_string(),
            "ENTRY-NEUTRAL".to_string(),
            "ENTRY-UNLABELLED".to_string()
        ],
        "the unlabelled row inherits the record's engagement; the row that declared \
         {ENGAGEMENT_A} keeps its own and stays out"
    );

    let bound_a = load(
        &service,
        RetrievalScope::bound(ENGAGEMENT_A).expect("engagement id binds"),
    )
    .await;
    assert_eq!(
        keys(&bound_a),
        vec!["ENTRY-A".to_string(), "ENTRY-NEUTRAL".to_string()],
        "an inherited label must not widen a row that named a different engagement"
    );
}

/// Pins the path that needs no tool call: memory injected into the system
/// prompt.
///
/// This is the shortest route the leak has. Nothing downstream of prompt
/// assembly inspects what the prompt already contains, so a filter that
/// covered `search_memory` and missed this one would leave the boundary open
/// while every tool-level test passed.
#[tokio::test]
async fn prompt_injected_memory_is_engagement_filtered() {
    let tmp = TempDir::new().expect("temp root");
    let service = AgentMemoryService::new(scoped_storage(&tmp));
    seed_notes(&service, None).await;
    let tiers = [notes_tier()];

    let mut unbound_request = MemoryRenderRequest::agent(SHARED_QUERY)
        .with_emit_audit(false)
        .with_temperature_overlay_repair(false);
    unbound_request.max_entries = 20;
    unbound_request.max_chars = 40_000;
    let unbound = magician::magician_v2::agents::render_memory_tiers_for_prompt_result(
        &service,
        AGENT,
        &tiers,
        &unbound_request,
    )
    .await
    .expect("unbound prompt render")
    .section
    .unwrap_or_default();
    assert!(
        unbound.contains("ENTRY-A") && unbound.contains("ENTRY-B"),
        "both engagements' notes must reach an unbound prompt, or the bound assertion below is \
         measuring an empty render. Got: {unbound}"
    );

    let mut bound_request = MemoryRenderRequest::agent(SHARED_QUERY)
        .with_emit_audit(false)
        .with_temperature_overlay_repair(false)
        .bound_to_engagement(RetrievalScope::bound(ENGAGEMENT_A).expect("engagement id binds"));
    bound_request.max_entries = 20;
    bound_request.max_chars = 40_000;
    let bound = magician::magician_v2::agents::render_memory_tiers_for_prompt_result(
        &service,
        AGENT,
        &tiers,
        &bound_request,
    )
    .await
    .expect("bound prompt render")
    .section
    .unwrap_or_default();
    assert!(
        bound.contains("ENTRY-A"),
        "the bound engagement's own note must still be injected. Got: {bound}"
    );
    assert!(
        !bound.contains("ENTRY-B"),
        "another engagement's note must never reach the prompt. Got: {bound}"
    );
    assert!(
        !bound.contains("ENTRY-UNLABELLED"),
        "an unlabelled note must not reach a bound prompt. Got: {bound}"
    );
}

/// Pins that the prompt-candidate snapshot cache cannot carry one
/// engagement's render into another's.
///
/// The snapshot cache is keyed by root/agent/scope/goal/tiers and
/// deliberately not by engagement. If containment were applied while building
/// that snapshot instead of while rendering from it, the FIRST engagement to
/// ask would write its filtered set into the shared cache and the second would
/// be served the first's answer — a leak with no store access at all. Running
/// A, then B, then A again against one warm cache is what catches that.
#[tokio::test]
async fn a_warm_prompt_snapshot_is_not_reused_across_engagements() {
    let tmp = TempDir::new().expect("temp root");
    let service = AgentMemoryService::new(scoped_storage(&tmp));
    seed_notes(&service, None).await;
    let tiers = [notes_tier()];

    let first_a = render_bound(&service, &tiers, ENGAGEMENT_A).await;
    let then_b = render_bound(&service, &tiers, ENGAGEMENT_B).await;
    let again_a = render_bound(&service, &tiers, ENGAGEMENT_A).await;

    assert!(
        first_a.contains("ENTRY-A") && !first_a.contains("ENTRY-B"),
        "cold render for A. Got: {first_a}"
    );
    assert!(
        then_b.contains("ENTRY-B") && !then_b.contains("ENTRY-A"),
        "B must not be served A's warm answer. Got: {then_b}"
    );
    assert!(
        again_a.contains("ENTRY-A") && !again_a.contains("ENTRY-B"),
        "A must not be served B's warm answer. Got: {again_a}"
    );
}

/// Render the agent tier for one engagement, with budgets wide enough that
/// nothing is dropped for size and the only reason an entry can be missing is
/// containment.
async fn render_bound(
    service: &AgentMemoryService,
    tiers: &[MemoryTierDefinition],
    engagement: &str,
) -> String {
    let mut request = MemoryRenderRequest::agent(SHARED_QUERY)
        .with_emit_audit(false)
        .with_temperature_overlay_repair(false)
        .bound_to_engagement(RetrievalScope::bound(engagement).expect("id binds"));
    request.max_entries = 20;
    request.max_chars = 40_000;
    magician::magician_v2::agents::render_memory_tiers_for_prompt_result(
        service, AGENT, tiers, &request,
    )
    .await
    .expect("bound prompt render")
    .section
    .unwrap_or_default()
}

/// Pins the runtime-owned argument the compiled `search_memory` /
/// `forget_memory` handlers read, including the two shapes that must NOT read
/// as "no engagement".
///
/// The executor stamps `__engagement_id` after stripping every model-supplied
/// `__*` key, so a present-but-unreadable value can only mean the runtime
/// wrote something wrong — and a handler that answered that with an unbound
/// search would hand back every engagement's memory on a malformed dispatch.
#[test]
fn compiled_handlers_read_the_engagement_from_runtime_arguments_only() {
    assert_eq!(
        retrieval_scope_from_runtime_args(&json!({ "query": "anything" })),
        Ok(RetrievalScope::Unbound),
        "no `__engagement_id` means the surface carries no engagement"
    );
    assert_eq!(
        retrieval_scope_from_runtime_args(&json!({ "__engagement_id": ENGAGEMENT_A })),
        Ok(RetrievalScope::Bound {
            engagement_id: ENGAGEMENT_A.to_string()
        })
    );
    assert!(
        retrieval_scope_from_runtime_args(&json!({ "__engagement_id": "" })).is_err(),
        "a blank engagement id must refuse, never fall back to an unbound search"
    );
    assert!(
        retrieval_scope_from_runtime_args(&json!({ "__engagement_id": ["a"] })).is_err(),
        "an unreadable engagement id must refuse, never fall back to an unbound search"
    );
}

/// Pins the corpora that have no labels to filter on: a bound execution is
/// refused, and an unbound one is untouched.
///
/// Refusal rather than a filtered read, because a "filtered" read of an
/// unlabelled corpus returns the whole corpus and reports success. The unbound
/// half of this test is what proves the gate is not simply refusing
/// everything.
#[test]
fn unlabelled_corpora_are_refused_under_a_bound_execution_and_open_otherwise() {
    for capability in ["search_notes", "read_file", "grep", "list_tasks", "files"] {
        assert!(
            cross_engagement_corpus_refusal(capability, &RetrievalScope::Unbound).is_none(),
            "`{capability}` must stay open to an execution carrying no engagement"
        );
        let refusal = cross_engagement_corpus_refusal(
            capability,
            &RetrievalScope::bound(ENGAGEMENT_A).expect("id binds"),
        )
        .unwrap_or_else(|| panic!("`{capability}` must be refused under a bound execution"));
        assert!(
            refusal.starts_with("NOT RETRIEVED"),
            "every refusal opens the same way so the model never has to work out which gate spoke"
        );
        assert!(refusal.contains(ENGAGEMENT_A));
    }

    // The acts an engagement exists to perform are not retrievals and must not
    // be caught by a retrieval gate.
    for capability in ["agentmail-send", "browser", "meeting", "search_memory"] {
        assert_eq!(
            cross_engagement_corpus_refusal(
                capability,
                &RetrievalScope::bound(ENGAGEMENT_A).expect("id binds")
            ),
            None,
            "`{capability}` is not a read of an unlabelled corpus"
        );
    }
}

/// Pins the browser boundary: two engagements never resolve to one
/// `agent-browser` session, and therefore never share a browser's cookies,
/// logged-in accounts or storage.
///
/// Asserting equality for the same engagement matters as much as inequality
/// across two: if the namespace were derived from anything per-call, a single
/// engagement's own steps would each open a fresh browser and the feature
/// would be broken in a way no absence-only assertion would notice.
#[test]
fn browser_sessions_are_partitioned_by_engagement() {
    let mut ctx =
        magician::magician_v2::execution::primitive_dispatch::exec_ctx::PrimitiveExecCtx::default_for_runtime();
    ctx.execution_id = Some("exec-1".to_string());

    let unbound_session = ctx.effective_browser_session_id();

    ctx.work_authority = Some(WorkAuthorityRef::from(&EngagementAuthorityRef {
        engagement_id: ENGAGEMENT_A.to_string(),
        authority_revision: 1,
    }));
    let a_session = ctx.effective_browser_session_id();
    let a_session_again = ctx.effective_browser_session_id();

    ctx.work_authority = Some(WorkAuthorityRef::from(&EngagementAuthorityRef {
        engagement_id: ENGAGEMENT_B.to_string(),
        authority_revision: 1,
    }));
    let b_session = ctx.effective_browser_session_id();

    assert_eq!(
        a_session, a_session_again,
        "one engagement must keep one browser across its own steps"
    );
    assert_ne!(
        a_session, b_session,
        "two engagements must never share a browser session id"
    );
    assert_ne!(
        a_session, unbound_session,
        "a bound execution must not land in the shared unbound browser"
    );
    assert!(
        a_session.contains("exec-1") && a_session.contains("northwind"),
        "the partitioned id must still name the execution it belongs to: {a_session}"
    );
}

/// Pins that an inherited browser-session override — the mechanism a delegated
/// child uses to share its parent's window — is namespaced rather than
/// honoured raw.
///
/// An override is a request to share a browser. Honouring one across
/// engagements is precisely the sharing that must not happen, and it is the
/// path a handover would take.
#[test]
fn an_inherited_browser_session_override_cannot_cross_engagements() {
    let mut ctx =
        magician::magician_v2::execution::primitive_dispatch::exec_ctx::PrimitiveExecCtx::default_for_runtime();
    ctx.browser_session_id_override = Some("magician-shared-window".to_string());

    ctx.work_authority = Some(WorkAuthorityRef::from(&EngagementAuthorityRef {
        engagement_id: ENGAGEMENT_A.to_string(),
        authority_revision: 1,
    }));
    let a_session = ctx.effective_browser_session_id();

    ctx.work_authority = Some(WorkAuthorityRef::from(&EngagementAuthorityRef {
        engagement_id: ENGAGEMENT_B.to_string(),
        authority_revision: 1,
    }));
    let b_session = ctx.effective_browser_session_id();

    assert_ne!(
        a_session, "magician-shared-window",
        "a bound execution must not attach to the raw shared window"
    );
    assert_ne!(
        a_session, b_session,
        "the same override handed to two engagements must resolve to two windows"
    );
    assert_eq!(
        engagement_browser_session_id(
            "magician-shared-window",
            &RetrievalScope::bound(ENGAGEMENT_A).expect("id binds")
        ),
        a_session,
        "the ctx must namespace the override through the same function everything else uses"
    );
}

/// Pins the unreadable-carrier case at the seam every render site goes
/// through: it yields no scope, so a render site renders nothing.
///
/// The tempting fallback — bind to some placeholder id — still admits every
/// `neutral` item, which would let an execution nobody could identify read the
/// one class of material an author marked shareable.
#[test]
fn an_unreadable_carrier_yields_no_scope_so_nothing_is_rendered() {
    assert_eq!(
        contained_retrieval_scope(Some(&EngagementAuthorityRef {
            engagement_id: "   ".to_string(),
            authority_revision: 1,
        })),
        None
    );
    assert_eq!(
        contained_retrieval_scope(None),
        Some(RetrievalScope::Unbound)
    );
    assert_eq!(
        contained_retrieval_scope(Some(&EngagementAuthorityRef {
            engagement_id: ENGAGEMENT_A.to_string(),
            authority_revision: 1,
        })),
        Some(RetrievalScope::Bound {
            engagement_id: ENGAGEMENT_A.to_string()
        })
    );
}

/// THE SECOND §5A.2 GAP, pinned at the two values that produce it: an execution
/// carrying no engagement is Unbound, and a browser call naming no mode is CDP.
///
/// `primitive_dispatch::dispatch` refuses CDP for a `Program` carrier outright,
/// and for an `Engagement` carrier when `contained_retrieval_scope(..)` reports
/// a BOUND scope or cannot be read at all. An autonomous agent cycle is created
/// with `work_authority: None` (`agents::autonomous_goal`), so it hits neither
/// arm: its scope is Unbound, `is_bound()` is false and the refusal does not
/// fire. The mode such a call then resolves to is
/// `Cdp` against the magicutor proxy, which attaches to the owner's OWN
/// signed-in Chrome — so an unbound agent's research visit carries the owner's
/// cookies, sessions and logged-in accounts to whatever site it opens. That is
/// the owner-identity inheritance §5A exists to prevent, on the one execution
/// shape an outward agent actually runs in.
///
/// Deliberately NOT an argument for extending the refusal to unbound
/// executions: unbound is also the owner's own chat, where attaching to his
/// Chrome is the point. The fix is a company-owned profile an agent can be
/// pinned to, which needs a `--user-data-dir`-shaped flag the vendored
/// `agent-browser` invocation does not pass. Until that exists this asserts
/// today's behaviour, so the day it changes the test says so.
///
/// HALF-CLOSED as of the per-agent browser-transport ceiling
/// (`AgentDefinition::browser_transports`): the carrier still reads Unbound and
/// the call still resolves to `Cdp`, both asserted below unchanged, but an
/// agent may now DECLARE that it cannot use `cdp` and the runtime refuses the
/// call regardless of what the model asked for or what the operator env
/// override says. That is the half this test used to record as impossible; the
/// remaining half is still open, because the ceiling withholds the owner's
/// identity without granting a company one.
#[test]
fn an_execution_with_no_engagement_still_browses_as_the_owner() {
    use magician::magician_v2::execution::primitive_dispatch::browser::session::{
        BrowserTransportCeiling, ConnectionMode, DEFAULT_MAGICUTOR_PROXY_URL,
        ENV_AGENT_BROWSER_MODE,
    };

    let unbound = contained_retrieval_scope(None).expect("no authority reads as unbound");
    assert_eq!(unbound, RetrievalScope::Unbound);
    assert!(
        !unbound.is_bound(),
        "the CDP refusal keys on `is_bound()`; false here is what lets the visit through"
    );

    assert!(
        std::env::var(ENV_AGENT_BROWSER_MODE).is_err(),
        "`{ENV_AGENT_BROWSER_MODE}` is set in this process, so the default this \
         test measures has been overridden and the measurement means nothing"
    );
    assert_eq!(
        ConnectionMode::from_call_arguments(&json!({}), DEFAULT_MAGICUTOR_PROXY_URL),
        ConnectionMode::Cdp {
            url: DEFAULT_MAGICUTOR_PROXY_URL.to_string()
        },
        "a browser call naming no connection_mode resolves to the owner's own Chrome"
    );
    // Avoiding it by naming a mode is the MODEL's choice, and a choice is not a
    // boundary.
    assert_eq!(
        ConnectionMode::from_call_arguments(
            &json!({ "connection_mode": "headless" }),
            DEFAULT_MAGICUTOR_PROXY_URL
        ),
        ConnectionMode::Headless
    );

    // The DEFINITION's choice is the boundary, and it outranks both of the
    // above. An agent declaring `browser_transports: [headless, headed]` cannot
    // reach the owner's Chrome even on the call that names nothing — the exact
    // call the first assertion just proved resolves to `Cdp`.
    let ceiling = BrowserTransportCeiling::parse(&["headless".to_string(), "headed".to_string()])
        .expect("declared ceiling parses");
    let defaulted = ConnectionMode::from_call_arguments(&json!({}), DEFAULT_MAGICUTOR_PROXY_URL);
    assert!(
        ceiling.resolve(defaulted).is_err(),
        "a declared ceiling must refuse the owner-identity transport, not substitute one"
    );
    assert_eq!(
        ceiling
            .resolve(ConnectionMode::Headless)
            .expect("a permitted transport passes through"),
        ConnectionMode::Headless
    );

    // And an agent that declares nothing is unchanged — the ceiling is opt-in.
    let unrestricted =
        BrowserTransportCeiling::parse::<String>(&[]).expect("an empty ceiling parses");
    assert_eq!(
        unrestricted
            .resolve(ConnectionMode::from_call_arguments(
                &json!({}),
                DEFAULT_MAGICUTOR_PROXY_URL
            ))
            .expect("unrestricted"),
        ConnectionMode::Cdp {
            url: DEFAULT_MAGICUTOR_PROXY_URL.to_string()
        }
    );
}
