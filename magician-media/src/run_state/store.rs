//! The run log — Module C.
//!
//! Append-only, one JSONL log per run: a form that spans days is one file, so
//! *"where did we get to"* is one read after any crash, restart or handover.
//! Current state is the fold of the log; nothing is updated in place, so the
//! log doubles as the history of what was drafted when — and, after
//! submission, as the evidence of what went out.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use magician::magician_v2::agents::ConsequenceClass;
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::audience::AudienceRef;
use magician::magician_v2::evidence::{
    OutwardActStatus, OutwardAssertionStore, OutwardChannel, OutwardScope, PrepareOutwardAct,
};

use super::types::{
    Answer, Expectation, ExpectationFulfilment, FieldState, Gap, GapResolution, Run, RunState,
    Submission,
};

const FIELD_SEP: char = '\u{1f}';

/// Scope for a store call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunScope {
    pub principal: String,
    pub workspace: String,
}

impl RunScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
        }
    }
}

/// What a submission must be able to **account for** before it happens.
///
/// Pressing send on a form is an outward act: something left, to someone, on
/// this exact payload, at this time. [`RunStateStore::record_submission`] has
/// always demanded the covering act's ref and never had a producer for one, so
/// the gate could not be satisfied honestly. This is what
/// [`RunStateStore::submit`] needs in order to record the act itself, on
/// [`OutwardChannel::Form`], before the run seals.
///
/// Generic by construction: nothing here names a kind of form, a programme or
/// a counterparty. Any flow that can say *what went out* and *who received it*
/// can submit through it.
pub struct FormSubmissionAct<'a> {
    /// The one authoritative copy of what was told to whom. Consumed, never
    /// owned.
    pub assertions: &'a OutwardAssertionStore,
    /// The exact artifact revision that is being submitted — immutable, never
    /// a "latest" pointer. What a person reviewed and what went out cannot
    /// diverge, and a disclosure that cannot say *what* was submitted cannot
    /// be corrected later.
    pub exact_payload_artifact_ref: &'a str,
    /// Who receives the submission — the portal's owner, the awarding body,
    /// whoever the form actually reaches.
    ///
    /// Never empty and never blank: an act naming no recipient would satisfy
    /// every recipient lookup vacuously, and a corrected answer would find
    /// nobody to tell.
    pub recipients: &'a [String],
}

/// A field as declared: its name, and whether the form demands it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FieldSpec {
    name: String,
    required: bool,
}

/// One line in a run's log.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum RunRecord {
    Opened {
        run_id: String,
        purpose: String,
        resource_ref: String,
        audience: Option<AudienceRef>,
        fields: Vec<FieldSpec>,
        opened_by: String,
        at: DateTime<Utc>,
    },
    FieldDeclared {
        name: String,
        required: bool,
        at: DateTime<Utc>,
    },
    AnswerRecorded {
        field: String,
        answer: Answer,
        at: DateTime<Utc>,
    },
    GapRaised {
        field: String,
        question: String,
        at: DateTime<Utc>,
    },
    GapResolved {
        field: String,
        question: String,
        answer: Answer,
        resolved_by: String,
        at: DateTime<Utc>,
    },
    ExpectationRaised {
        expectation_id: String,
        description: String,
        source_hint: String,
        at: DateTime<Utc>,
    },
    ExpectationFulfilled {
        expectation_id: String,
        event_ref: String,
        at: DateTime<Utc>,
    },
    Submitted {
        by: String,
        act_ref: String,
        at: DateTime<Utc>,
    },
}

/// Durable multi-session runs, one log per run.
///
/// Every input is SUPPLIED by the caller — answers, owner replies, arrived
/// events, the reviewed act behind a submission. The store reads no inbox, no
/// calendar, no other store and no config; that loose coupling is what lets
/// any flow use it, whatever channel its work actually happens on.
#[derive(Debug, Clone)]
pub struct RunStateStore {
    workspace_layout: ArtifactV2Workspace,
}

impl RunStateStore {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    /// The log's path. The file name is a hash of the run id rather than the
    /// id itself, so a caller-supplied id can never traverse out of the scope
    /// directory.
    fn run_path(&self, scope: &RunScope, run_id: &str) -> PathBuf {
        self.workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("run_state")
            .join(format!("{}.jsonl", stable_id(run_id)))
    }

    /// Open a run, or resume the one already open for this work.
    ///
    /// Idempotent on `(scope, purpose, resource_ref)`: a crashed session that
    /// calls `open` again lands on THE run — fields, answers, gaps and waits
    /// intact — rather than starting a duplicate beside it. §5: *"A form
    /// abandoned halfway is worse than one never started"*, and a duplicate
    /// half-form is abandonment with extra steps.
    ///
    /// On resume the supplied `fields`, `audience` and `opened_by` are ignored
    /// in favour of the first opening: the roster grows only through
    /// [`Self::declare_field`], so a resuming session with a stale view of the
    /// form cannot silently reshape it.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &self,
        scope: &RunScope,
        purpose: &str,
        resource_ref: &str,
        audience: Option<AudienceRef>,
        fields: &[(String, bool)],
        opened_by: &str,
        now: DateTime<Utc>,
    ) -> Result<Run> {
        if purpose.trim().is_empty() {
            anyhow::bail!(
                "a run must say what it is for; purpose is half of the run's identity, and \
                 blank would merge unrelated work against the same resource"
            );
        }
        if resource_ref.trim().is_empty() {
            anyhow::bail!(
                "a run must name the form or portal it runs against; without it a resumed \
                 session would have to guess which of the owner's applications this is"
            );
        }
        if purpose.contains(FIELD_SEP) || resource_ref.contains(FIELD_SEP) {
            anyhow::bail!(
                "a run's purpose and resource ref must not contain U+001F: it is the \
                 separator that keeps the id's components from bleeding into each other, and \
                 a component carrying it could fuse two different undertakings into one run"
            );
        }
        if scope.principal.contains(FIELD_SEP) || scope.workspace.contains(FIELD_SEP) {
            anyhow::bail!(
                "a scope's principal and workspace must not contain U+001F: it is the \
                 separator that keeps the run id's components from bleeding into each other, \
                 and a scope component carrying it could fuse two scopes' work into one run"
            );
        }
        if opened_by.trim().is_empty() {
            anyhow::bail!("a run must record who opened it");
        }
        if let Some(bound) = &audience {
            if !bound.is_named() {
                anyhow::bail!(
                    "a bound audience must be named; an unnamed relationship is open-ended \
                     access by another name, which this codebase refuses to represent"
                );
            }
        }
        let mut seen_names = BTreeSet::new();
        for (name, _) in fields {
            if name.trim().is_empty() {
                anyhow::bail!("a field must have a name; a blank field can never be answered");
            }
            if name.contains(FIELD_SEP) {
                anyhow::bail!(
                    "a field name must not contain U+001F: it is the separator that keeps a \
                     gap key's components from bleeding into each other, and a field name \
                     carrying it could fuse two different gaps into one"
                );
            }
            if !seen_names.insert(name.as_str()) {
                anyhow::bail!(
                    "field `{name}` is declared twice in one opening; two fields with one name \
                     would leave an answer with no single home"
                );
            }
        }

        let run_id = derive_run_id(scope, purpose, resource_ref);
        if let Some(existing) = self.load(scope, &run_id)? {
            return Ok(existing);
        }

        let run = Run {
            run_id: run_id.clone(),
            purpose: purpose.trim().to_string(),
            resource_ref: resource_ref.trim().to_string(),
            audience: audience.clone(),
            fields: fields
                .iter()
                .map(|(name, required)| FieldState {
                    name: name.clone(),
                    required: *required,
                    answer: None,
                    revision: 0,
                    updated_at: now,
                })
                .collect(),
            gaps: Vec::new(),
            expectations: Vec::new(),
            submission: None,
            opened_at: now,
            opened_by: opened_by.to_string(),
        };
        self.append(
            &self.run_path(scope, &run_id),
            &RunRecord::Opened {
                run_id,
                purpose: run.purpose.clone(),
                resource_ref: run.resource_ref.clone(),
                audience,
                fields: fields
                    .iter()
                    .map(|(name, required)| FieldSpec {
                        name: name.clone(),
                        required: *required,
                    })
                    .collect(),
                opened_by: run.opened_by.clone(),
                at: now,
            },
        )?;
        Ok(run)
    }

    /// Declare a field the form revealed after opening — a conditional
    /// section.
    ///
    /// Idempotent on the name: re-declaring an existing field returns the run
    /// unchanged, and the first declaration — including its `required` flag —
    /// wins, so a resumed session re-noticing the same section cannot reshape
    /// it. The new field arrives pending, at revision `0`.
    pub fn declare_field(
        &self,
        scope: &RunScope,
        run_id: &str,
        name: &str,
        required: bool,
        now: DateTime<Utc>,
    ) -> Result<Run> {
        if name.trim().is_empty() {
            anyhow::bail!("a field must have a name; a blank field can never be answered");
        }
        if name.contains(FIELD_SEP) {
            anyhow::bail!(
                "a field name must not contain U+001F: it is the separator that keeps a \
                 gap key's components from bleeding into each other, and a field name \
                 carrying it could fuse two different gaps into one"
            );
        }
        let run = self.require(scope, run_id)?;
        Self::refuse_after_submission(&run)?;
        if run.field(name).is_some() {
            return Ok(run);
        }
        self.append(
            &self.run_path(scope, run_id),
            &RunRecord::FieldDeclared {
                name: name.to_string(),
                required,
                at: now,
            },
        )?;
        self.reload(scope, run_id)
    }

    /// Record a grounded answer into a declared field.
    ///
    /// Overwriting with a different answer is allowed any time before
    /// submission — the log keeps every revision as history — and each such
    /// write bumps the field's revision, which is what [`Run::changed_since`]
    /// reads. Replaying the answer the field already holds is idempotent,
    /// like every other replayable write here: a crashed session re-running
    /// its steps appends nothing and bumps nothing, so a watermark held
    /// across the replay never reports a change that never happened.
    ///
    /// An answer with no evidence refs is refused outright, never stored: §5,
    /// *"every emitted answer cites retrievable evidence — never answer from
    /// nothing"*. A caller holding an ungroundable question records a gap via
    /// [`Self::raise_gap`] instead — *"an invented metric on a real
    /// application is unrecoverable in a way a missed deadline is not."*
    pub fn record_answer(
        &self,
        scope: &RunScope,
        run_id: &str,
        field: &str,
        answer: &Answer,
        now: DateTime<Utc>,
    ) -> Result<Run> {
        let run = self.require(scope, run_id)?;
        Self::refuse_after_submission(&run)?;
        if run.field(field).is_none() {
            anyhow::bail!(
                "field `{field}` was never declared on run `{run_id}`; declare it first — \
                 accepting it silently would let a typo create a phantom field no review sees"
            );
        }
        Self::refuse_ungrounded(field, answer)?;
        if run
            .field(field)
            .is_some_and(|held| held.answer.as_ref() == Some(answer))
        {
            // Idempotent replay: the identical answer is already the field's
            // truth, and re-recording it would burn a revision on a no-op —
            // every watermark-based consumer would re-process an unchanged
            // field.
            return Ok(run);
        }
        self.append(
            &self.run_path(scope, run_id),
            &RunRecord::AnswerRecorded {
                field: field.to_string(),
                answer: answer.clone(),
                at: now,
            },
        )?;
        self.reload(scope, run_id)
    }

    /// Turn a question the evidence cannot ground into an owner-facing gap.
    ///
    /// §5: *"a question the evidence store cannot ground becomes an
    /// owner-facing gap, never synthesised text"*. Idempotent on
    /// `(field, question)`: the same unknown re-noticed by a resumed session
    /// must not double the owner's queue.
    pub fn raise_gap(
        &self,
        scope: &RunScope,
        run_id: &str,
        field: &str,
        question: &str,
        now: DateTime<Utc>,
    ) -> Result<Run> {
        if question.trim().is_empty() {
            anyhow::bail!(
                "a gap must carry the question the owner is being asked; a blank question \
                 cannot be answered"
            );
        }
        if field.contains(FIELD_SEP) || question.contains(FIELD_SEP) {
            anyhow::bail!(
                "a gap's field and question must not contain U+001F: it is the separator \
                 that keeps the gap key's components from bleeding into each other, and a \
                 component carrying it could fuse two different unknowns into one gap"
            );
        }
        let run = self.require(scope, run_id)?;
        Self::refuse_after_submission(&run)?;
        if run.field(field).is_none() {
            anyhow::bail!(
                "field `{field}` was never declared on run `{run_id}`; a gap must hang off the \
                 field whose answer it is blocking, or its resolution would have nowhere to land"
            );
        }
        if find_gap(&run, field, question).is_some() {
            return Ok(run);
        }
        self.append(
            &self.run_path(scope, run_id),
            &RunRecord::GapRaised {
                field: field.to_string(),
                question: question.trim().to_string(),
                at: now,
            },
        )?;
        self.reload(scope, run_id)
    }

    /// Resolve a gap with the owner's reply, by name.
    ///
    /// §5: *"The owner's reply is new evidence"* — the reply's own ref belongs
    /// in the answer's `evidence_refs`, which is what lets an owner-supplied
    /// fact clear the same grounding bar as everything else. The grounded
    /// answer is also written into the gap's field — one truth, never a
    /// resolved gap sitting beside a stale field.
    #[allow(clippy::too_many_arguments)]
    pub fn resolve_gap(
        &self,
        scope: &RunScope,
        run_id: &str,
        field: &str,
        question: &str,
        answer: &Answer,
        resolved_by: &str,
        now: DateTime<Utc>,
    ) -> Result<Run> {
        if field.contains(FIELD_SEP) || question.contains(FIELD_SEP) {
            anyhow::bail!(
                "a gap's field and question must not contain U+001F: it is the separator \
                 that keeps the gap key's components from bleeding into each other, and a \
                 component carrying it could fuse two different unknowns into one gap"
            );
        }
        let run = self.require(scope, run_id)?;
        Self::refuse_after_submission(&run)?;
        let Some(gap) = find_gap(&run, field, question) else {
            anyhow::bail!(
                "no gap on `{field}` asks that question; a reply must land on the question \
                 that raised it, or an answer to one unknown quietly fills another"
            );
        };
        if let Some(resolution) = &gap.resolution {
            anyhow::bail!(
                "that gap was already resolved by `{}`; the owner's first reply is the record, \
                 and a later one must not silently replace it",
                resolution.resolved_by
            );
        }
        if resolved_by.trim().is_empty() {
            anyhow::bail!(
                "a gap resolution must name who answered; an unattributed resolution is \
                 synthesised text wearing an owner's authority"
            );
        }
        Self::refuse_ungrounded(field, answer)?;
        let record = RunRecord::GapResolved {
            field: gap.field.clone(),
            question: gap.question.clone(),
            answer: answer.clone(),
            resolved_by: resolved_by.to_string(),
            at: now,
        };
        self.append(&self.run_path(scope, run_id), &record)?;
        self.reload(scope, run_id)
    }

    /// Raise an out-of-band expectation: the run now waits on an event that
    /// will arrive somewhere else — an inbox, a phone.
    ///
    /// The module NEVER reads that somewhere. §5's *"the agent must read its
    /// own inbox mid-flow, extract the code, and continue"* is the CALLER's
    /// loop; [`Self::fulfill_expectation`] is where the extracted event comes
    /// back in. That decoupling is what makes the primitive generic.
    ///
    /// Idempotent on `(description, source_hint)` within the run: ids are
    /// derived, so the same wait re-raised by a resumed session is the same
    /// expectation — not a second one holding the run hostage forever.
    pub fn raise_expectation(
        &self,
        scope: &RunScope,
        run_id: &str,
        description: &str,
        source_hint: &str,
        now: DateTime<Utc>,
    ) -> Result<Expectation> {
        if description.trim().is_empty() {
            anyhow::bail!(
                "an expectation must describe what it is waiting for; a blank wait can never \
                 be recognised when its event arrives"
            );
        }
        if source_hint.trim().is_empty() {
            anyhow::bail!(
                "an expectation must say where its event will arrive; a wait nobody knows \
                 where to watch never gets fed"
            );
        }
        if description.contains(FIELD_SEP) || source_hint.contains(FIELD_SEP) {
            anyhow::bail!(
                "an expectation's description and source hint must not contain U+001F: it \
                 is the separator that keeps the id's components from bleeding into each \
                 other, and a component carrying it could fuse two different waits into one \
                 expectation"
            );
        }
        let run = self.require(scope, run_id)?;
        Self::refuse_after_submission(&run)?;
        let expectation_id = derive_expectation_id(run_id, description, source_hint);
        if let Some(existing) = run
            .expectations
            .iter()
            .find(|held| held.expectation_id == expectation_id)
        {
            return Ok(existing.clone());
        }
        self.append(
            &self.run_path(scope, run_id),
            &RunRecord::ExpectationRaised {
                expectation_id: expectation_id.clone(),
                description: description.trim().to_string(),
                source_hint: source_hint.trim().to_string(),
                at: now,
            },
        )?;
        self.reload(scope, run_id)?
            .expectations
            .into_iter()
            .find(|held| held.expectation_id == expectation_id)
            .context("expectation vanished immediately after being raised")
    }

    /// Feed the arrived event to the wait it satisfies.
    ///
    /// An unknown id is refused — feeding an event to a wait this run never
    /// raised is how a stray email resumes the wrong run — and so is a second
    /// fulfilment: which event satisfied a wait is a fact, and a later event
    /// must not rewrite it.
    pub fn fulfill_expectation(
        &self,
        scope: &RunScope,
        run_id: &str,
        expectation_id: &str,
        event_ref: &str,
        now: DateTime<Utc>,
    ) -> Result<Run> {
        let run = self.require(scope, run_id)?;
        Self::refuse_after_submission(&run)?;
        if event_ref.trim().is_empty() {
            anyhow::bail!(
                "a fulfilment must cite the event that arrived; a wait closed by nothing is an \
                 unverified verification"
            );
        }
        let Some(expectation) = run
            .expectations
            .iter()
            .find(|held| held.expectation_id == expectation_id)
        else {
            anyhow::bail!(
                "no expectation `{expectation_id}` on run `{run_id}`; feeding an event to a \
                 wait this run never raised is how a stray email resumes the wrong run"
            );
        };
        if let Some(fulfilment) = &expectation.fulfilled {
            anyhow::bail!(
                "expectation `{expectation_id}` was already fulfilled by `{}`; which event \
                 satisfied a wait is a fact, and a second event must not rewrite it",
                fulfilment.event_ref
            );
        }
        self.append(
            &self.run_path(scope, run_id),
            &RunRecord::ExpectationFulfilled {
                expectation_id: expectation_id.to_string(),
                event_ref: event_ref.to_string(),
                at: now,
            },
        )?;
        self.reload(scope, run_id)
    }

    /// Record that a person pressed send.
    ///
    /// §5: *"Submission is a submission-class act: covered only by a reviewed
    /// batch, never a standing envelope. The agent drafts the whole thing; a
    /// person presses send."* This is the module's only path to `submitted`,
    /// and it does not exist without a named person and the reviewed act's ref
    /// — an unnamed submission is exactly how an agent would fire the gate
    /// itself. Refused unless the run is ready for review: submitting an
    /// ungrounded run is the disaster case the gate protects against. The
    /// first submission stands; a second is refused.
    ///
    /// The already-submitted refusal narrows, but cannot close, the racing
    /// double-submit: two sessions can both read `submission: None` and both
    /// append, because the store is append-only files with no lock — there is
    /// no atomic compare-and-append to hand, the same shape the approval
    /// envelopes store documents on its debit path. What converges the log is
    /// the fold: the FIRST `Submitted` record wins and every later one is
    /// ignored, so the durable record names exactly one submission however
    /// the race interleaves. A losing racer's `Ok` therefore carries the
    /// WINNER's submission — a caller must read the returned run rather than
    /// assume its own arguments landed.
    ///
    /// This is the **primitive**: it takes the covering act's ref from
    /// whatever register issued it and never reaches for one itself, which is
    /// what keeps the run usable by any flow. [`Self::submit`] is the path
    /// that records the act too, and it is the one to reach for unless the
    /// caller already holds a ref it can point at.
    pub fn record_submission(
        &self,
        scope: &RunScope,
        run_id: &str,
        by: &str,
        act_ref: &str,
        now: DateTime<Utc>,
    ) -> Result<Run> {
        let run = self.require(scope, run_id)?;
        Self::refuse_unsubmittable(&run, by)?;
        if act_ref.trim().is_empty() {
            anyhow::bail!(
                "a submission must cite the reviewed act that covered it; a submission-class \
                 act is covered only by a reviewed batch, never a standing envelope"
            );
        }
        self.append(
            &self.run_path(scope, run_id),
            &RunRecord::Submitted {
                by: by.to_string(),
                act_ref: act_ref.to_string(),
                at: now,
            },
        )?;
        self.reload(scope, run_id)
    }

    /// Submit the run: record the outward act, **then** seal the run.
    ///
    /// The accountable path to `submitted`, and the one that closes the gap
    /// [`Self::record_submission`] left open. `record_submission` has always
    /// demanded the covering act's ref, and nothing recorded an act on
    /// [`OutwardChannel::Form`] — so the only way to satisfy the gate was to
    /// hand it a ref pointing at nothing. This prepares the act itself.
    ///
    /// # Record before the act, or no act
    ///
    /// The whole submit gate runs first, before a single line reaches the
    /// register: an act prepared for a run that then refuses to submit would
    /// be a record of a submission that never happened, which is as much a lie
    /// as a submission with no record. Then, in order:
    ///
    /// 1. **prepare** the `Form` act — the §4 step that must succeed *before*
    ///    anything leaves. If it fails, this returns `Err` and the run is not
    ///    submitted: the whole point of preparing first is to have something
    ///    to fail closed on.
    /// 2. **mark dispatching** — the run is about to say a person pressed
    ///    send, so the act is in flight. Left at `Prepared` it would read as
    ///    *"nothing has left yet"*, which `is_active_disclosure` excludes: a
    ///    corrected figure would never find the body that received the form.
    ///    Only advanced from `Prepared`, so a resumed run climbs no second
    ///    ladder.
    /// 3. **seal the run** by feeding the act's own ref into
    ///    `record_submission`.
    ///
    /// `ProviderAccepted` is deliberately **not** claimed. There is no
    /// provider receipt for a form the way there is for an email gateway, and
    /// inventing one would put fabricated evidence of acceptance in the
    /// permanent record. A caller holding a real confirmation — a reference
    /// number, an acknowledgement mail — records it with
    /// `record_provider_receipt`; a caller that cannot tell says so with
    /// `mark_dispatch_unknown`.
    ///
    /// A crash between steps 2 and 3 leaves an act in flight with no
    /// submission: honest, and exactly what `unreconciled` exists to surface.
    /// The reverse order has no honest reading — a sealed run whose disclosure
    /// was never recorded is a submission nobody can account for.
    ///
    /// The outward scope is derived from the run's scope rather than supplied
    /// beside it: they are the same principal and workspace by definition, and
    /// a caller free to hand in a different one could file a run's submission
    /// where no reverse lookup for that run will ever go.
    pub fn submit(
        &self,
        scope: &RunScope,
        run_id: &str,
        by: &str,
        act: &FormSubmissionAct<'_>,
        now: DateTime<Utc>,
    ) -> Result<Run> {
        let run = self.require(scope, run_id)?;
        // The entire gate, before the register is touched — see the fn doc.
        Self::refuse_unsubmittable(&run, by)?;

        if act.exact_payload_artifact_ref.trim().is_empty() {
            anyhow::bail!(
                "a submission act must name the exact artifact revision that went out; a \
                 disclosure that cannot say WHAT was submitted cannot be corrected later"
            );
        }
        if act.recipients.is_empty() {
            anyhow::bail!(
                "a submission act must name who receives it; an act with no recipient \
                 satisfies every recipient lookup vacuously, so a corrected answer would \
                 find nobody to tell"
            );
        }
        if act.recipients.iter().any(|held| held.trim().is_empty()) {
            anyhow::bail!(
                "a submission act's recipients must each be named; a blank recipient is an \
                 unaddressed disclosure wearing a real one's shape"
            );
        }
        // The run id is the whole of the act's separator-joined idempotency
        // key. An id carrying the separator could shift bytes across it and
        // fuse two runs' submissions into one act — after which the register's
        // answer to "what did we send them" is wrong rather than missing.
        if run.run_id.contains(FIELD_SEP) {
            anyhow::bail!(
                "run id `{}` contains U+001F: it is the separator the submission act's \
                 idempotency key is built from, and an id carrying it could fuse two runs' \
                 submissions into one act",
                run.run_id.escape_debug()
            );
        }

        let outward_scope = OutwardScope::new(scope.principal.clone(), scope.workspace.clone());
        // NEITHER work field, and the audience carried instead.
        //
        // This used to route `run.audience` onto whichever work field matched
        // its kind, reasoning that an id "rides on the field matching its
        // kind". The kinds match; the ID SPACES do not. An `AudienceRef` has no
        // contract about what its id names, and the one thing that resolves an
        // engagement-kind audience to members —
        // `CounterpartyAudiences::living_audience` — reads it with
        // `counterparty_store.load`. So the id is a COUNTERPARTY id, while
        // `engagement_id` is the value every reverse lookup under
        // `WorkContextKind::ENGAGEMENT_TOKEN` asks with. Writing one into the
        // other filed a submission under a linkage that does not exist, and
        // `reindex_work_axes` then counted it as correctly attributed — so the
        // count that separates "the index is stale" from "this was never
        // attributed" answered confidently and wrongly.
        //
        // The same guess was removed from `data_room::disclosure_bridge`; this
        // was its twin, and leaving it would have been the fix moving rather
        // than landing.
        let request = PrepareOutwardAct {
            idempotency_key: submission_idempotency_key(&run.run_id),
            program_id: None,
            engagement_id: None,
            exact_payload_artifact_ref: act.exact_payload_artifact_ref.to_string(),
            // The named person who pressed send is who asserted it. The module
            // never fires the gate itself, so there is no other honest sender.
            effective_sender: by.to_string(),
            intended_audience: act.recipients.to_vec(),
            channel: OutwardChannel::Form,
            // §5: *"Submission is a submission-class act"* — fixed here rather
            // than chosen by the caller, because a caller free to downgrade it
            // could have a standing envelope cover a submission.
            consequence_class: ConsequenceClass::SubmissionOrPublication
                .as_str()
                .to_string(),
        };
        // One instant in the two forms the two stores speak — derived, never
        // taken twice, so a submission cannot be recorded at one moment and
        // sealed at another.
        let now_rfc3339 = now.to_rfc3339();
        // Filed by the audience when the run names one, so the act is still
        // findable by relationship — which is what the work fields were
        // wrongly doing. A run with no audience prepares as before.
        let prepared = match run.audience.as_ref() {
            Some(audience) => act.assertions.prepare_for_audience(
                &outward_scope,
                &request,
                audience,
                &now_rfc3339,
            )?,
            None => act
                .assertions
                .prepare(&outward_scope, &request, &now_rfc3339)?,
        };
        if prepared.status == OutwardActStatus::Prepared {
            act.assertions.mark_dispatching(
                &outward_scope,
                &prepared.outward_act_ref,
                &now_rfc3339,
            )?;
        }
        self.record_submission(scope, run_id, by, &prepared.outward_act_ref, now)
    }

    /// Every run this scope has opened, oldest first.
    ///
    /// A run id is DERIVED from `(scope, purpose, resource_ref)` and nothing
    /// keeps an index of them, so an owner who cannot restate that tuple
    /// character-for-character cannot reach their own half-filled form. This
    /// is the listing that closes it: the file name is a hash of the id, but
    /// every log's `Opened` record carries the id in full, so folding the
    /// directory recovers what the derivation cannot be asked backwards for.
    ///
    /// # Absent is empty; unreadable is not
    ///
    /// A scope that has never opened a run has no directory, and that is the
    /// only condition that reads as *"no runs"*. Every other listing failure
    /// propagates, and so does a log that will not fold: a listing that
    /// silently dropped the run it could not read would tell an owner a form
    /// holding days of answers does not exist, and `open` would then
    /// fabricate a blank one beside it.
    ///
    /// A log that vanished between the listing and the read contributes
    /// nothing — it holds no run now — rather than failing the whole listing.
    ///
    /// Ordered by `opened_at` then `run_id`, so two runs opened in one instant
    /// still come back in one fixed order.
    pub fn all_runs(&self, scope: &RunScope) -> Result<Vec<Run>> {
        let root = self
            .workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("run_state");
        let mut out = Vec::new();
        for path in magician::magician_v2::jsonl::list_log_paths(&self.workspace_layout, &root)? {
            let Some(raw) = self.read_if_present(&path)? else {
                continue;
            };
            if let Some(run) = fold_run(&raw, &path)? {
                out.push(run);
            }
        }
        out.sort_by(|left, right| {
            left.opened_at
                .cmp(&right.opened_at)
                .then_with(|| left.run_id.cmp(&right.run_id))
        });
        Ok(out)
    }

    /// One run, folded from its log; `None` if it was never opened.
    pub fn load(&self, scope: &RunScope, run_id: &str) -> Result<Option<Run>> {
        let path = self.run_path(scope, run_id);
        let Some(raw) = self.read_if_present(&path)? else {
            return Ok(None);
        };
        fold_run(&raw, &path)
    }

    fn require(&self, scope: &RunScope, run_id: &str) -> Result<Run> {
        self.load(scope, run_id)?.with_context(|| {
            format!(
                "no run `{run_id}` in this scope; refusing to write into a run that was never \
                 opened — a mistyped id here would scatter one form's state across two logs"
            )
        })
    }

    fn reload(&self, scope: &RunScope, run_id: &str) -> Result<Run> {
        self.load(scope, run_id)?
            .context("run vanished immediately after being written")
    }

    fn refuse_after_submission(run: &Run) -> Result<()> {
        if let Some(submission) = &run.submission {
            anyhow::bail!(
                "run `{}` was submitted by `{}`; after submission the log is the evidence of \
                 what actually went out, and evidence does not get edited",
                run.run_id,
                submission.by
            );
        }
        Ok(())
    }

    /// Every refusal a submission faces that does not depend on the covering
    /// act's ref.
    ///
    /// Shared rather than duplicated so [`Self::submit`] can run the entire
    /// gate BEFORE it prepares anything in the register. A copy here would be
    /// a copy that drifts, and the drift would be a run that
    /// `record_submission` refuses while an act already claims it went out.
    fn refuse_unsubmittable(run: &Run, by: &str) -> Result<()> {
        // Check-then-append: this refusal narrows the double-submit race but
        // cannot close it — see `record_submission`'s doc. The fold's
        // first-record-wins arm is what converges the log when two racers both
        // get past here.
        if let Some(submission) = &run.submission {
            anyhow::bail!(
                "run `{}` was already submitted by `{}`; the first submission stands — a \
                 second send on a submitted form is the event the gate exists to prevent",
                run.run_id,
                submission.by
            );
        }
        if by.trim().is_empty() {
            anyhow::bail!(
                "a submission must name the person who pressed send; the module never fires \
                 the gate itself, and an unnamed submission is how an agent would"
            );
        }
        if run.state() != RunState::ReadyForReview {
            let required_total = run.fields.iter().filter(|held| held.required).count();
            let unanswered_required = run
                .fields
                .iter()
                .filter(|held| held.required && held.answer.is_none())
                .count();
            let open_required_gaps = run
                .gaps
                .iter()
                .filter(|gap| {
                    gap.is_open() && run.field(&gap.field).is_some_and(|held| held.required)
                })
                .count();
            let open_expectations = run
                .expectations
                .iter()
                .filter(|held| held.is_open())
                .count();
            anyhow::bail!(
                "run `{}` is not ready for review ({unanswered_required} of \
                 {required_total} required fields unanswered, {open_required_gaps} open gaps \
                 on required fields, {open_expectations} open expectations, {} fields declared \
                 in total); submitting an ungrounded run is the disaster case the gate \
                 protects against",
                run.run_id,
                run.fields.len()
            );
        }
        Ok(())
    }

    fn refuse_ungrounded(field: &str, answer: &Answer) -> Result<()> {
        if !answer.is_grounded() {
            anyhow::bail!(
                "refusing an answer for `{field}` that cites no retrievable evidence: an \
                 invented metric on a real application is unrecoverable in a way a missed \
                 deadline is not — record a gap and ask the owner instead"
            );
        }
        Ok(())
    }

    fn append(&self, path: &PathBuf, record: &RunRecord) -> Result<()> {
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        magician::magician_v2::jsonl::append_log_line(&self.workspace_layout, path, &line)
            .with_context(|| format!("appending {}", path.display()))?;
        Ok(())
    }

    fn read_if_present(&self, path: &PathBuf) -> Result<Option<String>> {
        // NotFound is the only error that reads as an absent run. Everything
        // else propagates: an unreadable log folded to "never opened" would
        // have a probe deny the run exists — and `open` would fabricate a
        // blank run over a form holding days of answers. Shared semantics
        // live in `magician_v2::jsonl`.
        magician::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, path)
    }
}

/// The fold: one linear pass, position maps for in-place updates, and a
/// defensive twin of every write-side refusal — a duplicate or invalid record
/// written by an older binary must not change what the run says happened.
/// Line-level failures are decided in `magician_v2::jsonl`: a torn tail is an
/// append that never happened, and an unparseable interior line is corruption
/// the fold refuses to guess past.
fn fold_run(raw: &str, path: &Path) -> Result<Option<Run>> {
    let mut run: Option<Run> = None;
    let mut field_pos: HashMap<String, usize> = HashMap::new();
    let mut gap_pos: HashMap<String, usize> = HashMap::new();
    let mut expectation_pos: HashMap<String, usize> = HashMap::new();
    // One monotonic change sequence across the whole run — see the note on
    // `FieldState::revision` for why revisions are not per-field counters.
    let mut next_revision: u64 = 1;

    // Tolerant of a torn tail only — see `magician_v2::jsonl`. Without this,
    // one session crashing mid-append would brick the run for every session
    // after it: load() errors, and open() — which loads first — errors too,
    // so the log this module keeps precisely to survive a crash would become
    // unreadable BECAUSE of one.
    for record in magician::magician_v2::jsonl::parse_log_lines::<RunRecord>(raw, path)? {
        match record {
            RunRecord::Opened {
                run_id,
                purpose,
                resource_ref,
                audience,
                fields,
                opened_by,
                at,
            } => {
                if run.is_some() {
                    // The first opening wins; a duplicate line must not reset
                    // the run.
                    continue;
                }
                let mut states: Vec<FieldState> = Vec::new();
                for spec in fields {
                    if field_pos.contains_key(&spec.name) {
                        // Defensive: the write refuses duplicate names, and
                        // the fold must too.
                        continue;
                    }
                    field_pos.insert(spec.name.clone(), states.len());
                    states.push(FieldState {
                        name: spec.name,
                        required: spec.required,
                        answer: None,
                        revision: 0,
                        updated_at: at,
                    });
                }
                run = Some(Run {
                    run_id,
                    purpose,
                    resource_ref,
                    audience,
                    fields: states,
                    gaps: Vec::new(),
                    expectations: Vec::new(),
                    submission: None,
                    opened_at: at,
                    opened_by,
                });
            },
            RunRecord::FieldDeclared { name, required, at } => {
                let Some(held) = run.as_mut() else { continue };
                if held.submission.is_some() || field_pos.contains_key(&name) {
                    // Frozen after submission; and the first declaration —
                    // including its shape — wins.
                    continue;
                }
                field_pos.insert(name.clone(), held.fields.len());
                held.fields.push(FieldState {
                    name,
                    required,
                    answer: None,
                    revision: 0,
                    updated_at: at,
                });
            },
            RunRecord::AnswerRecorded { field, answer, at } => {
                let Some(held) = run.as_mut() else { continue };
                // Defensive twins of the write-side refusals: after submission
                // the log is evidence, an ungrounded answer is unrepresentable
                // as stored state, and an undeclared field has no home for one.
                if held.submission.is_some() || !answer.is_grounded() {
                    continue;
                }
                let Some(&pos) = field_pos.get(&field) else {
                    continue;
                };
                let held_field = &mut held.fields[pos];
                held_field.answer = Some(answer);
                held_field.revision = next_revision;
                held_field.updated_at = at;
                next_revision += 1;
            },
            RunRecord::GapRaised {
                field,
                question,
                at,
            } => {
                let Some(held) = run.as_mut() else { continue };
                if held.submission.is_some() || !field_pos.contains_key(&field) {
                    continue;
                }
                let key = gap_key(&field, &question);
                if gap_pos.contains_key(&key) {
                    // The first raising wins; the same unknown re-noticed is
                    // not a second question.
                    continue;
                }
                gap_pos.insert(key, held.gaps.len());
                held.gaps.push(Gap {
                    field,
                    question,
                    raised_at: at,
                    resolution: None,
                });
            },
            RunRecord::GapResolved {
                field,
                question,
                answer,
                resolved_by,
                at,
            } => {
                let Some(held) = run.as_mut() else { continue };
                if held.submission.is_some()
                    || resolved_by.trim().is_empty()
                    || !answer.is_grounded()
                {
                    continue;
                }
                let Some(&gap_at) = gap_pos.get(&gap_key(&field, &question)) else {
                    continue;
                };
                if held.gaps[gap_at].resolution.is_some() {
                    // The owner's first reply is the record.
                    continue;
                }
                held.gaps[gap_at].resolution = Some(GapResolution {
                    answer: answer.clone(),
                    resolved_by,
                    at,
                });
                // One truth: the grounded reply is also the field's answer.
                if let Some(&pos) = field_pos.get(&field) {
                    let held_field = &mut held.fields[pos];
                    held_field.answer = Some(answer);
                    held_field.revision = next_revision;
                    held_field.updated_at = at;
                    next_revision += 1;
                }
            },
            RunRecord::ExpectationRaised {
                expectation_id,
                description,
                source_hint,
                at,
            } => {
                let Some(held) = run.as_mut() else { continue };
                if held.submission.is_some() || expectation_pos.contains_key(&expectation_id) {
                    continue;
                }
                expectation_pos.insert(expectation_id.clone(), held.expectations.len());
                held.expectations.push(Expectation {
                    expectation_id,
                    description,
                    source_hint,
                    raised_at: at,
                    fulfilled: None,
                });
            },
            RunRecord::ExpectationFulfilled {
                expectation_id,
                event_ref,
                at,
            } => {
                let Some(held) = run.as_mut() else { continue };
                if held.submission.is_some() || event_ref.trim().is_empty() {
                    continue;
                }
                let Some(&pos) = expectation_pos.get(&expectation_id) else {
                    continue;
                };
                if held.expectations[pos].fulfilled.is_some() {
                    // The first event to land is the one that satisfied the
                    // wait.
                    continue;
                }
                held.expectations[pos].fulfilled = Some(ExpectationFulfilment { event_ref, at });
            },
            RunRecord::Submitted { by, act_ref, at } => {
                let Some(held) = run.as_mut() else { continue };
                if held.submission.is_some() {
                    // First submission wins — defensively, and as the arbiter
                    // of `record_submission`'s check-then-append race: when
                    // two racers both pass the write-side check and both
                    // append, this line is what converges the log on one
                    // submission.
                    continue;
                }
                if by.trim().is_empty() || act_ref.trim().is_empty() {
                    // Defensive: an unnamed or uncovered submission never
                    // lands, however it got into the log.
                    continue;
                }
                held.submission = Some(Submission { by, act_ref, at });
            },
        }
    }
    Ok(run)
}

fn stable_id(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..32].to_string()
}

/// Collapse whitespace and case, for identity components that are prose.
fn normalized(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

/// A gap's identity within its run: the field it blocks plus the normalised
/// question, so the same question re-noticed with different spacing or case is
/// the same gap.
///
/// The separator claim is enforced at every write that feeds this key: field
/// names are refused it where they are declared ([`RunStateStore::open`],
/// [`RunStateStore::declare_field`]), and both components where gaps are
/// raised and resolved.
fn gap_key(field: &str, question: &str) -> String {
    format!("{field}{FIELD_SEP}{}", normalized(question))
}

fn find_gap<'held>(run: &'held Run, field: &str, question: &str) -> Option<&'held Gap> {
    let key = gap_key(field, question);
    run.gaps
        .iter()
        .find(|gap| gap_key(&gap.field, &gap.question) == key)
}

/// The id for one run.
///
/// Derived — never allocated — from `(principal, workspace, purpose,
/// resource_ref)`, joined by a separator no component contains, so a crashed
/// session that opens again resumes THE run instead of duplicating it:
///
/// - `principal` and `workspace` — scope isolation; the same work by two
///   principals must never share a half-filled form.
/// - `purpose` — two different undertakings against the same portal are two
///   runs. Normalised for whitespace and case, because a resumed session
///   re-stating its purpose rarely comes back character-identical.
/// - `resource_ref` — the same purpose against two different forms must not
///   merge. Trimmed only: it is a reference, not prose.
///
/// The separator claim is enforced, not assumed: [`RunStateStore::open`]
/// refuses a scope component, purpose or resource ref containing the
/// separator, so no crafted component can shift bytes across it and fuse two
/// identities into one id.
///
/// `audience` is deliberately NOT in the tuple: it describes who the run
/// serves, and a resumed session that can no longer re-supply the binding must
/// still land on the run it opened. Were an audience ever added here, it would
/// have to enter via `as_key()` — kind included — never its bare id.
fn derive_run_id(scope: &RunScope, purpose: &str, resource_ref: &str) -> String {
    format!(
        "run-{}",
        stable_id(&format!(
            "{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}",
            scope.principal,
            scope.workspace,
            normalized(purpose),
            resource_ref.trim(),
        ))
    )
}

/// The stable key for one run's submission act — the run id, and nothing else.
///
/// A run submits **once**: `RunState::Submitted` is terminal and the fold's
/// first-record-wins arm keeps it that way, so one run can only ever have one
/// submission to account for. Keying on the run id alone is what makes the
/// §4 promise structural — *"an idempotent retry RESUMES the same record,
/// never duplicates it"* — for a resumed session that prepared the act and
/// died before the run could seal.
///
/// Deliberately NOT folding in the payload ref, the recipients or the clock: a
/// retry that re-derives any of them slightly differently would prepare a
/// SECOND act for one submission, and the register would report the form going
/// out twice. Scope does not repeat here either — the store folds principal
/// and workspace into the final act ref itself.
///
/// The separator claim is enforced at the write: [`RunStateStore::submit`]
/// refuses a run id containing U+001F, so no crafted id can shift bytes across
/// the separator and fuse two runs' submissions into one act.
fn submission_idempotency_key(run_id: &str) -> String {
    format!("form_submission{FIELD_SEP}{run_id}")
}

/// The id for one wait.
///
/// Derived from `(run_id, description, source_hint)`:
///
/// - `run_id` — already carries the scope, and pins the wait to its run, so
///   two runs awaiting identically described events keep separate waits.
/// - `description` — what is awaited; normalised like `purpose`, for the same
///   reason.
/// - `source_hint` — the same words awaited on two different channels are two
///   different waits. A hint is an address, so it is trimmed but not
///   case-folded.
///
/// The separator claim is enforced, not assumed: [`RunStateStore::raise_expectation`]
/// refuses a description or source hint containing the separator, and `run_id`
/// is a derived `run-<hex>` token that cannot carry it.
fn derive_expectation_id(run_id: &str, description: &str, source_hint: &str) -> String {
    format!(
        "exp-{}",
        stable_id(&format!(
            "{run_id}{FIELD_SEP}{}{FIELD_SEP}{}",
            normalized(description),
            source_hint.trim(),
        ))
    )
}
