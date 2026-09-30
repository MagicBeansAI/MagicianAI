//! The going-dark lane for the unified runtime activity view.
//!
//! # What this file is for
//!
//! A view that silently stops receiving from a worker looks exactly like a
//! quiet system. Add a background worker without a span, move a boundary, or
//! let the layer stop emitting, and nothing fails today — the view just shows
//! less, and "less" and "nothing is happening" render identically.
//!
//! This is the same role the tool-skill canary lane plays. That whole effort
//! exists because a runtime rewrite took twenty-one of sixty-four skills dark
//! while every test stayed green. **Something has to fail when this view goes
//! dark.** That is the only job of this file.
//!
//! # Why these tests drive production functions and not `info_span!`
//!
//! `runtime_activity_layer`'s own unit tests already cover the layer against
//! synthetic spans — roughly thirty of them, and they are the right place for
//! layer semantics. Repeating that here would produce a file that passes
//! forever while every real boundary rots away, which is precisely the failure
//! being guarded against.
//!
//! So every test below except the span-floor half drives a real production
//! function carrying a real `#[instrument]`, and **fails if that attribute is
//! deleted**. The exact mutation that must break each test is written on the
//! test itself; see "Proving the guard works" below.
//!
//! # Which workers are driven, and why not only memory consolidation
//!
//! The plan names memory consolidation, and
//! `MemoryConsolidator::consolidate_cycle_completed_v3` is driven here as the
//! primary case — it is a real production boundary and it runs offline with a
//! temp-dir memory service, no LLM router and no prompt manager, exactly as its
//! own unit tests construct it.
//!
//! It cannot, however, supply the *child* half of the contract. The only spans
//! nested under a consolidation cycle are `llm_dispatch` and `llm_chunking_run`,
//! both of which need a bound LLM router; with `llm_router: None` the cycle
//! opens one span and closes it. A tree needs a second boundary, so the nesting
//! tests drive the enrichment pipeline instead:
//!
//! * `EnrichmentPipeline::run` (`slot_graph/enrichment.rs`) opens a production
//!   `info_span!("slot_enrichment", …)` and — the part that matters — wraps its
//!   `await` in `.instrument(span)`. That is the production code that decides
//!   whether the view renders a tree or a flat list.
//! * `memory_applicability::judge_or_fall_back` (`memory_applicability.rs`) is a
//!   second real instrumented worker that runs completely offline: with no
//!   router bound it returns the candidate order unchanged without reaching a
//!   provider, while its `#[instrument]` span still opens and closes.
//!
//! The enricher joining them is written in this file, but it is the pipeline's
//! own public extension point (`SlotEnricher`), not a test double standing in
//! for instrumentation: **both spans in that tree are opened by production
//! code**, and the parent/child edge is created by production's
//! `.instrument(span)`. Delete either attribute and the test fails.
//!
//! # Proving the guard works — nobody has watched these fail yet
//!
//! Each test carries a `MUTATION:` note naming the single edit that must break
//! it and the assertion that must fire. Whoever runs this lane first should
//! spend a minute confirming at least the first one, because a guard nobody has
//! seen fail is a guard nobody should trust.
//!
//! # How a vacuous pass is prevented
//!
//! The classic empty-collection pass — "collect events, assert each one
//! matches" — succeeds on zero events. Two things stop it here:
//!
//! 1. Every test asserts a **count before it inspects anything**.
//!    `expect_at_least` fails on a short drain with a message naming what was
//!    expected, and each boundary assertion is an `assert_eq!` on a *filtered*
//!    count of exactly one, which no empty vector can satisfy. Filtered counts
//!    rather than the raw length, because analytics row emission inside the
//!    consolidation cycle may add unrelated progress rows and this lane must not
//!    fail for that.
//! 2. `uninstrumented_production_work_produces_no_activity_start` is the
//!    negative control: it runs real production async code carrying no span and
//!    asserts zero records, then asserts exactly one record from an emission in
//!    the same test. A harness that observes nothing passes the first half and
//!    fails the second.
//!
//! The related trap is a filter that silently drops everything. `TEST_TARGET` is
//! rooted at `magician::` deliberately: `should_skip_target` admits only
//! first-party target roots, so a bare `runtime_activity_test` root would be
//! dropped as third-party and every assertion here would observe an empty
//! vector. The production targets driven below (`magician::magician_v2::…`)
//! satisfy the same allowlist by construction.

use std::sync::Arc;

use async_trait::async_trait;
use magician::magician_v2::{
    agents::{
        memory::AgentMemoryService, memory_consolidator::MemoryConsolidator, types::AgentDefinition,
    },
    analytics::runtime_activity_layer::{
        ActivityChannel, ActivityRecord, RuntimeActivityLayer, KIND_BACKGROUND, KIND_RUNTIME,
    },
    artifact_v2::{
        memory::V3EpisodeRecord,
        workspace::{DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE},
    },
    memory_applicability::judge_or_fall_back,
    slot_graph::{
        EnrichmentContext, EnrichmentOutcome, EnrichmentPipeline, SlotEnricher, SlotRecord,
        SlotType,
    },
};
use tracing_subscriber::layer::SubscriberExt;

/// Target for the one deliberately synthetic emission in this file — the span
/// floor, which is *about* the absence of a span and so cannot be expressed
/// with one.
///
/// Rooted at `magician` because `should_skip_target` admits only first-party
/// roots. A bare `runtime_activity_visibility` root would be dropped as
/// third-party, and that test would then assert about an empty vector instead
/// of about the layer.
const TEST_TARGET: &str = "magician::runtime_activity_visibility_test";

// ─────────────────────────────────────────────────────────────────────
// Harness
// ─────────────────────────────────────────────────────────────────────

/// Run `f` on a current-thread runtime under a registry carrying only the
/// activity layer, then return everything the layer queued.
///
/// This mirrors `runtime_activity_layer`'s own `record_activity` helper rather
/// than inventing a second harness — same private channel per call, same drain
/// — and adds the one thing an integration test needs that the unit tests do
/// not: a runtime, because every worker driven here is an `async fn`.
///
/// The runtime is **current-thread, and driven inside `with_default`**, on
/// purpose. `with_default` installs a thread-local subscriber, so a
/// multi-threaded runtime would poll these futures on worker threads that never
/// see it and every test would silently observe zero records — the exact
/// vacuous pass this file is written against.
///
/// Capacity is generous for the same reason: `ActivityChannel` evicts oldest on
/// overflow, so a tight capacity would drop the parent `Started` and turn a
/// real failure into a confusing one.
fn record_activity<Fut>(f: impl FnOnce() -> Fut) -> Vec<ActivityRecord>
where
    Fut: std::future::Future<Output = ()>,
{
    let channel = Arc::new(ActivityChannel::new(512));
    let subscriber = tracing_subscriber::registry()
        .with(RuntimeActivityLayer::with_channel(Arc::clone(&channel)));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime for the activity harness");
    tracing::subscriber::with_default(subscriber, || runtime.block_on(f()));

    let mut records = Vec::new();
    while let Some(record) = channel.try_recv() {
        records.push(record);
    }
    records
}

/// The fields these tests read off an `ActivityStarted`.
struct Start {
    activity_id: u64,
    parent_activity_id: Option<u64>,
    kind: &'static str,
    principal: Option<String>,
    workspace: Option<String>,
}

/// Fail before inspecting anything when the drain is short.
///
/// This is the guard against the empty-collection pass. It runs *first* in
/// every test, and its message names what was expected rather than reporting a
/// bare length, so a failure reads as "the worker went dark" and not as an
/// off-by-one.
#[track_caller]
fn expect_at_least(records: &[ActivityRecord], minimum: usize, expectation: &str) {
    assert!(
        !records.is_empty(),
        "the activity layer recorded NOTHING. Expected {expectation}. Either the \
         production span is gone, its target is no longer first-party, or the \
         harness never observed the subscriber."
    );
    assert!(
        records.len() >= minimum,
        "expected at least {minimum} activity records ({expectation}), got {}: {records:#?}",
        records.len()
    );
}

/// Every `Started` carrying `name`.
fn starts_named(records: &[ActivityRecord], name: &str) -> Vec<Start> {
    let mut found = Vec::new();
    for record in records {
        if let ActivityRecord::Started {
            activity_id,
            parent_activity_id,
            name: candidate,
            kind,
            principal,
            workspace,
            ..
        } = record
        {
            if *candidate == name {
                found.push(Start {
                    activity_id: *activity_id,
                    parent_activity_id: *parent_activity_id,
                    // Copied out of the reference, not coerced through it:
                    // `kind` is `&'static str` on the record, and borrowing it
                    // instead would tie `Start` to the drained vector.
                    kind: *kind,
                    principal: principal.clone(),
                    workspace: workspace.clone(),
                });
            }
        }
    }
    found
}

/// The single `Started` carrying `name`, or a failure naming the worker.
///
/// `assert_eq!` on a filtered count of exactly one is what makes this
/// non-vacuous: no empty vector can satisfy it.
#[track_caller]
fn only_start(records: &[ActivityRecord], name: &str) -> Start {
    let mut matches = starts_named(records, name);
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one ActivityStarted named `{name}` — the worker's boundary \
         span. Found {}. Records: {records:#?}",
        matches.len()
    );
    matches.remove(0)
}

/// The duration and outcome of the `Finished` closing `activity_id`.
#[track_caller]
fn finish_for(records: &[ActivityRecord], activity_id: u64, name: &str) -> (u64, String) {
    let mut matches = Vec::new();
    for record in records {
        if let ActivityRecord::Finished {
            activity_id: found,
            duration_ms,
            outcome,
            ..
        } = record
        {
            if *found == activity_id {
                matches.push((*duration_ms, (*outcome).to_string()));
            }
        }
    }
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one ActivityFinished closing `{name}` (activity \
         {activity_id}). A start with no finish means the view shows a unit that \
         never ends. Records: {records:#?}"
    );
    matches.remove(0)
}

/// Assert a started row routes to the default scope.
///
/// Pinned rather than merely observed. Undeclared work used to fall back to
/// `system`/`system`, which forced every activity view to open a second
/// subscription to a cross-principal bucket just to see ordinary background
/// work. The fallback is now `anonymous`/`default`; a silent revert would make
/// these rows invisible to the view without failing anything else.
#[track_caller]
fn assert_default_scope(start: &Start, what: &str) {
    assert_eq!(
        (start.principal.as_deref(), start.workspace.as_deref()),
        (Some(DEFAULT_SCOPE_PRINCIPAL), Some(DEFAULT_SCOPE_WORKSPACE)),
        "{what} must route to the default scope (`{DEFAULT_SCOPE_PRINCIPAL}`/\
         `{DEFAULT_SCOPE_WORKSPACE}`) when it declares none. `system`/`system` is \
         reserved for work that says so positively, and an unscoped row is \
         invisible to every subscriber."
    );
}

// ─────────────────────────────────────────────────────────────────────
// Fixtures for the memory consolidation worker
// ─────────────────────────────────────────────────────────────────────

/// The smallest definition the consolidation cycle accepts.
///
/// Built from YAML rather than a struct literal on purpose: `AgentDefinition`
/// gains fields regularly, and a literal here would break this lane for reasons
/// that have nothing to do with runtime visibility. This is also the shape
/// `memory_consolidator`'s own unit tests use.
///
/// It declares **no** `principal` and **no** `workspace`, which is the point:
/// the span then has nothing to declare and nothing to inherit, so it exercises
/// the default-scope fallback the view depends on.
fn undeclared_agent_definition() -> AgentDefinition {
    serde_yaml::from_str(
        r#"
agent_id: "activity-visibility-agent"
name: "Activity Visibility Agent"
persona: "Drives the runtime activity going-dark lane"
tools: []
"#,
    )
    .expect("minimal AgentDefinition YAML should parse")
}

/// A completed episode for the cycle to consolidate.
///
/// Deserialized rather than constructed for the same reason as the definition
/// above, and additionally because `V3EpisodeRecord` carries thirty-odd fields;
/// a constructor call here would pin an arity this lane has no opinion about.
///
/// `outcome_kind` must not be `paused` — the cycle returns early on a paused
/// episode, which would still emit the boundary span but for a reason that
/// hides a real regression behind an early return.
fn completed_episode(agent_id: &str, goal_id: &str) -> V3EpisodeRecord {
    let now = chrono::Utc::now().to_rfc3339();
    serde_json::from_value(serde_json::json!({
        "agent_id": agent_id,
        "episode_id": "episode-activity-visibility",
        "goal_key": goal_id,
        "consolidation_key": "cycle_completed",
        "trigger_type": "manual",
        "trigger_seq": 1,
        "trigger_timestamp": now,
        "started_at": now,
        "completed_at": now,
        "outcome_kind": "goal_achieved",
        "outcome_summary": "the going-dark lane drove one consolidation cycle",
        "root_execution_id": null,
        "parent_execution_id": null,
        "task_agent_output_id": null,
        "task_user_output_id": null,
    }))
    .expect("minimal V3EpisodeRecord JSON should deserialize")
}

// ─────────────────────────────────────────────────────────────────────
// Enrichers — the pipeline's own extension point, used to reach a second
// production boundary from inside a production span.
// ─────────────────────────────────────────────────────────────────────

/// Calls a real instrumented worker from inside the pipeline's span.
///
/// The call happens across the pipeline's `await`, which is the whole point:
/// `EnrichmentPipeline::run` wraps that await in `.instrument(span)`, and if it
/// did not, the judge's span would open with no current span and land at the
/// root of the view as a sibling instead of a child.
struct JudgeInvokingEnricher;

#[async_trait]
impl SlotEnricher for JudgeInvokingEnricher {
    fn name(&self) -> &'static str {
        "activity_visibility_judge_caller"
    }

    /// Empty means "willing to see every slot", per the trait's own contract.
    fn supported_types(&self) -> &'static [SlotType] {
        &[]
    }

    async fn enrich(
        &self,
        _slot: &mut SlotRecord,
        _context: &EnrichmentContext,
    ) -> anyhow::Result<EnrichmentOutcome> {
        // A real production worker on its offline path: with no router bound
        // and fewer than two candidates it returns the incoming order untouched
        // without reaching a provider. The `#[instrument]` span still opens and
        // closes, which is what this test reads.
        let _unchanged = judge_or_fall_back(
            Vec::new(),
            "does the runtime activity view still see nested work",
            None,
            DEFAULT_SCOPE_PRINCIPAL,
            DEFAULT_SCOPE_WORKSPACE,
        )
        .await;
        Ok(EnrichmentOutcome::unchanged())
    }
}

/// Fails every slot, so production's own `error!` fires inside the span.
struct FailingEnricher;

#[async_trait]
impl SlotEnricher for FailingEnricher {
    fn name(&self) -> &'static str {
        "activity_visibility_failing"
    }

    fn supported_types(&self) -> &'static [SlotType] {
        &[]
    }

    async fn enrich(
        &self,
        _slot: &mut SlotRecord,
        context: &EnrichmentContext,
    ) -> anyhow::Result<EnrichmentOutcome> {
        Err(anyhow::anyhow!(
            "activity visibility lane: deliberate failure on slot {}",
            context.slot_id
        ))
    }
}

fn one_slot() -> Vec<SlotRecord> {
    vec![SlotRecord {
        id: "activity_visibility_slot".to_string(),
        slot_type: SlotType::Entity,
        value: serde_json::json!({"name": "runtime activity"}),
        confidence: 0.5,
        provenance: vec![],
        evidence_links: vec![],
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }]
}

// ─────────────────────────────────────────────────────────────────────
// The tests
// ─────────────────────────────────────────────────────────────────────

/// The primary going-dark guard: the background worker the plan names is
/// visible in the view.
///
/// Drives `MemoryConsolidator::consolidate_cycle_completed_v3` — the real
/// production entry point, constructed exactly as its own unit tests construct
/// it (temp-dir memory service, no LLM router, no prompt manager) so it runs
/// with no network and no credentials.
///
/// The definition carries no consolidation rules, so the cycle does no tier
/// work. That is deliberate: the assertion is about the *boundary*, and a rule
/// that needed an LLM would make this lane fail for provider reasons rather
/// than visibility ones. The cycle still crosses a real `await` into the tier
/// interpreter, which is what an `async fn`'s span has to survive.
///
/// MUTATION: delete the `#[instrument(name = "memory_consolidation_cycle", …)]`
/// attribute above `consolidate_cycle_completed_v3` in
/// `magician/src/magician_v2/agents/memory_consolidator.rs` (currently ~line
/// 884). `expect_at_least` then fires with "the activity layer recorded
/// NOTHING". Changing only the span's `name` fails `only_start` instead, with
/// "expected exactly one ActivityStarted named `memory_consolidation_cycle`".
#[test]
fn memory_consolidation_cycle_is_visible_as_an_activity_row() {
    let temp = tempfile::tempdir().expect("temp dir for the memory service");
    // Owned, and moved into the future rather than borrowed from the enclosing
    // scope: the harness takes `impl FnOnce() -> Fut`, and a future borrowing a
    // by-value closure capture is the one shape that needs an async closure.
    let base = temp.path().to_path_buf();
    let agent_id = "activity-visibility-agent";
    let goal_id = "activity-visibility-goal";

    let records = record_activity(move || async move {
        let memory = AgentMemoryService::with_base_path(&base);
        let consolidator = MemoryConsolidator::new(memory, None, None);
        let definition = undeclared_agent_definition();
        let episode = completed_episode(agent_id, goal_id);

        consolidator
            .consolidate_cycle_completed_v3(&definition, agent_id, goal_id, &episode)
            .await
            .expect("one consolidation cycle over an empty rule set should succeed");
    });

    expect_at_least(
        &records,
        2,
        "an ActivityStarted and an ActivityFinished for `memory_consolidation_cycle`",
    );

    let start = only_start(&records, "memory_consolidation_cycle");

    assert_eq!(
        start.parent_activity_id, None,
        "a worker driven with nothing above it is a root in the view, not a child"
    );
    assert_eq!(
        start.kind, KIND_BACKGROUND,
        "memory consolidation declares `activity_kind = KIND_BACKGROUND`; the view \
         groups background work by it, and losing the declaration silently \
         reclassifies the row"
    );
    assert_default_scope(&start, "an undeclared consolidation cycle");

    let (duration_ms, outcome) =
        finish_for(&records, start.activity_id, "memory_consolidation_cycle");
    // Not `> 0`: a fast cycle legitimately rounds to zero milliseconds. What
    // matters is that a duration was computed and carried, which is the field
    // the view reads to stop rendering the unit as still running.
    assert!(
        duration_ms < 60_000,
        "a local no-op consolidation cycle reported {duration_ms}ms, which is not a \
         duration this cycle can have — the finish is closing the wrong start"
    );
    assert_eq!(
        outcome, "closed",
        "an undeclared outcome must not read as success: the layer watched the span \
         close, it did not watch the worker succeed"
    );
}

/// The tree: a nested worker hangs off its parent, and the edge survives an
/// `await`.
///
/// Both spans are production. `EnrichmentPipeline::run` opens
/// `info_span!("slot_enrichment", …)` and wraps its await in
/// `.instrument(span)`; `judge_or_fall_back` opens its own via `#[instrument]`.
/// The enricher joining them is written in this file, but it is the pipeline's
/// public `SlotEnricher` extension point and it contributes no span of its own.
///
/// This is the assertion that separates a tree from a flat list. A span that
/// does not cover its awaits still emits `Started` and `Finished` — it simply
/// stops being the current span at the first suspension point, so everything
/// beneath it arrives parentless and the view degrades to a list of roots with
/// nothing failing.
///
/// MUTATION (parent side): drop `.instrument(span)` from
/// `EnrichmentPipeline::run` in
/// `magician/src/magician_v2/slot_graph/enrichment.rs` (currently ~line 265) and
/// await the block bare. The `slot_enrichment` row still appears, but the child
/// assertion fires with "expected `memory_applicability_judge` to hang off
/// `slot_enrichment`".
///
/// MUTATION (child side): delete the `#[instrument(name =
/// "memory_applicability_judge", …)]` attribute above `judge_or_fall_back` in
/// `magician/src/magician_v2/memory_applicability.rs` (currently ~line 278).
/// `only_start` then fires with "expected exactly one ActivityStarted named
/// `memory_applicability_judge`".
#[test]
fn a_nested_worker_hangs_off_its_parent_across_an_await() {
    let records = record_activity(|| async {
        let enrichers: Vec<Arc<dyn SlotEnricher>> = vec![Arc::new(JudgeInvokingEnricher)];
        let pipeline = EnrichmentPipeline::new(enrichers);
        let mut slots = one_slot();
        let _summary = pipeline.run(&mut slots).await;
    });

    expect_at_least(
        &records,
        4,
        "a start and finish for `slot_enrichment` and for the \
         `memory_applicability_judge` nested inside it",
    );

    let parent = only_start(&records, "slot_enrichment");
    let child = only_start(&records, "memory_applicability_judge");

    assert_eq!(
        parent.parent_activity_id, None,
        "the pipeline span is the root here"
    );
    assert_eq!(
        child.parent_activity_id,
        Some(parent.activity_id),
        "expected `memory_applicability_judge` to hang off `slot_enrichment` \
         (activity {}), got {:?}. A parentless child means the production span \
         stopped covering its await, and the view renders a flat list instead of a \
         tree.",
        parent.activity_id,
        child.parent_activity_id
    );
    assert_ne!(
        child.activity_id, parent.activity_id,
        "parent and child must be distinct units"
    );

    // Kind is what the view groups by, and these two boundaries resolve it by
    // different routes: the judge declares `activity_kind` explicitly, the
    // pipeline span declares nothing and falls back to the target heuristic.
    // Asserting both keeps a change to either route visible.
    assert_eq!(
        parent.kind, KIND_RUNTIME,
        "an undeclared span under `slot_graph::enrichment` falls back to the runtime \
         family"
    );
    assert_eq!(
        child.kind, KIND_BACKGROUND,
        "`judge_or_fall_back` declares `activity_kind = KIND_BACKGROUND`"
    );

    assert_default_scope(&parent, "an undeclared enrichment span");

    let (_, parent_outcome) = finish_for(&records, parent.activity_id, "slot_enrichment");
    let (_, child_outcome) = finish_for(&records, child.activity_id, "memory_applicability_judge");
    assert_eq!(parent_outcome, "closed");
    assert_eq!(child_outcome, "closed");

    // The child must close before its parent. A view drawing a child still
    // running inside a finished parent is showing a boundary that does not
    // enclose what it claims to.
    let position = |target: u64| {
        records
            .iter()
            .position(|record| match record {
                ActivityRecord::Finished { activity_id, .. } => *activity_id == target,
                _ => false,
            })
            .expect("both finishes were asserted present above")
    };
    assert!(
        position(child.activity_id) < position(parent.activity_id),
        "the nested worker must close before the boundary that contains it"
    );
}

/// The negative control, and the span floor.
///
/// Two halves that only pass together, which is what makes this a control
/// rather than a second happy path:
///
/// * Real production async code carrying no span — the enrichment pipeline with
///   no enrichers configured — must produce **zero** records. A harness that
///   over-collects fails here.
/// * An INFO line emitted outside every span must produce **exactly one**
///   record, a loose `ActivityProgress` with no `activity_id`. A harness that
///   observes nothing at all fails here.
///
/// Without the second half, a harness whose subscriber was never installed
/// would sail through the first and look identical to a passing test. Without
/// the first, an assertion about "the worker emits" could not distinguish
/// emission from noise.
///
/// The loose line is the one deliberately synthetic emission in this file: the
/// span floor is *about* code running outside any span, so no production
/// boundary could express it. Rows arriving with no `activity_id` are honest,
/// not a bug — the view renders them at the root, and
/// `ActivityProgress::activity_id` is optional for exactly this reason.
///
/// MUTATION: make `RuntimeActivityLayer::on_event` return early when
/// `ctx.event_span(event)` is `None` (a plausible "tidy up orphan rows"
/// change). The loose half then drops to zero records and fires "expected an
/// INFO line outside every span to arrive as one loose ActivityProgress".
#[test]
fn uninstrumented_production_work_produces_no_activity_start() {
    let quiet = record_activity(|| async {
        // A real production async call with no span anywhere on its path: the
        // pipeline breaks out of its loop before opening `slot_enrichment` when
        // no enrichers are configured.
        let pipeline = EnrichmentPipeline::new(Vec::new());
        let mut slots = one_slot();
        let _summary = pipeline.run(&mut slots).await;
    });
    assert!(
        quiet.is_empty(),
        "production code that opens no span must contribute no activity records; got \
         {quiet:#?}. Records appearing here mean the harness is collecting something \
         other than what the tests above measure."
    );

    let loose = record_activity(|| async {
        tracing::info!(target: TEST_TARGET, "a line emitted outside every span");
    });
    assert_eq!(
        loose.len(),
        1,
        "expected an INFO line outside every span to arrive as one loose \
         ActivityProgress, got {loose:#?}. Zero here means the harness never observed \
         the layer at all, which would make every other assertion in this file \
         vacuous."
    );
    assert!(
        !loose
            .iter()
            .any(|record| matches!(record, ActivityRecord::Started { .. })),
        "a bare log line must not manufacture an ActivityStarted: {loose:#?}"
    );
    match &loose[0] {
        ActivityRecord::Progress {
            activity_id,
            level,
            message,
            principal,
            workspace,
            ..
        } => {
            assert_eq!(
                *activity_id, None,
                "a line below the span floor has no unit to hang off; the view renders \
                 it loose at the root rather than inventing a parent"
            );
            assert_eq!(*level, "info");
            assert_eq!(message, "a line emitted outside every span");
            // Loose rows carry no scope, so they reach a viewer through the
            // unscoped path rather than the default-scope fallback that spans
            // get. Pinned so the two cases cannot drift into each other.
            assert_eq!((principal.as_deref(), workspace.as_deref()), (None, None));
        },
        other => panic!("expected a loose ActivityProgress, got {other:?}"),
    }
}

/// An ERROR raised by production code inside a production span marks that unit
/// failed, and the row lands on it rather than at the root.
///
/// This is the attribution half of the contract, and a distinct regression from
/// the tree: a unit that visibly contains a red row while closing `closed`
/// tells an operator the worker succeeded when it did not. The `error!` here is
/// production's own, in `EnrichmentPipeline::run`'s enricher-failure arm — this
/// file only supplies an enricher that fails.
///
/// MUTATION: change `MarkError::Yes` to `MarkError::No` in
/// `RuntimeActivityLayer::on_event`'s level check
/// (`magician/src/magician_v2/analytics/runtime_activity_layer.rs`, currently
/// ~line 1059). The progress row still arrives and still attributes, but the
/// outcome assertion fires with "a unit containing an ERROR must not close as
/// `closed`". Downgrading production's `error!` to `warn!` in `enrichment.rs`
/// fails the same assertion.
#[test]
fn an_error_inside_a_worker_span_marks_that_unit_failed() {
    let records = record_activity(|| async {
        let enrichers: Vec<Arc<dyn SlotEnricher>> = vec![Arc::new(FailingEnricher)];
        let pipeline = EnrichmentPipeline::new(enrichers);
        let mut slots = one_slot();
        let _summary = pipeline.run(&mut slots).await;
    });

    expect_at_least(
        &records,
        3,
        "a start, an error progress row and a finish for `slot_enrichment`",
    );

    let start = only_start(&records, "slot_enrichment");

    let mut error_rows = Vec::new();
    for record in &records {
        if let ActivityRecord::Progress {
            activity_id, level, ..
        } = record
        {
            if *level == "error" {
                error_rows.push(*activity_id);
            }
        }
    }
    assert_eq!(
        error_rows.len(),
        1,
        "expected production's own `error!` inside the enrichment span to arrive as \
         one ActivityProgress. Records: {records:#?}"
    );
    assert_eq!(
        error_rows[0],
        Some(start.activity_id),
        "the error row must hang off the span it was logged in, not float at the root"
    );

    let (_, outcome) = finish_for(&records, start.activity_id, "slot_enrichment");
    assert_eq!(
        outcome, "error",
        "a unit containing an ERROR must not close as `closed` — a green row wrapping \
         a red one is how a failing worker reads as healthy"
    );
}
