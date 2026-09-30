//! The controller loop.
//!
//! ```text
//! claim lease (fenced)
//!   → resolve policy
//!   → no required checks?           → UNVERIFIED (explicit, never a silent pass)
//!   → capture immutable snapshot    → the attestation key
//!   → key already green?            → VERIFIED, without copying anything
//!   → reap stranded workspaces, materialise this one
//!   → run the entire resolved policy
//!   → tracked source mutated?       → INDETERMINATE, release the lease, re-run
//!   → append attempt, accept result (write-once, fenced)
//!       ├─ green  → finalise under compare-and-set → VERIFIED
//!       ├─ red    → budget/no-progress → REPAIRING or EXHAUSTED
//!       └─ indet. → UNAVAILABLE (fails closed; never a pass)
//! ```
//!
//! Every branch that cannot reach a confident green ends somewhere that is not
//! green. That is the whole invariant: an unavailable evidence store, an
//! unreadable policy, a crashed attempt or a mutated snapshot must never read
//! as "checked and fine".
//!
//! Two properties hold across *every* one of those branches, including the
//! ones that end by erroring:
//!
//! * **the lease goes back.** A lease belongs to a pass, not to a gate, and a
//!   pass that keeps it after finishing locks the gate out for the rest of the
//!   TTL — fatal on the repair path, where the successor arrives seconds later
//!   under a fresh worker id. A renewal task keeps the lease live for exactly
//!   as long as the pass runs; the thing actually preventing a second
//!   worker's write is still the generation fence, see [`super::store`].
//! * **store calls made *by a pass* do not run on a tokio worker.** Every
//!   mutating call takes a blocking `flock` and some replay a journal, so
//!   every one on a pass path goes through `VerificationController::on_store`.
//!
//! One method on this type is the deliberate exception, and it is not on a
//! pass path: [`VerificationController::cancel`] calls `self.deps.store`
//! directly on the caller's thread — including the `flock` — because it has
//! no production caller to be blocking. It is a settle path that this rule
//! does *not* cover; if it ever acquires one, it has to move to `on_store`
//! with the rest.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};

use super::attestation::{
    AcceptedResult, AttemptOutcome, AttestationKey, VerificationAttempt, VerificationAttestation,
};
use super::events::{VerificationEvent, VerificationEventKind, VerificationEventSink};
use super::gate::{GateStatus, VerificationGate};
use super::ids::{AttemptId, GateId};
use super::journal::JournalPayload;
use super::policy::{resolve, VerificationPolicy};
use super::repair::{self, FailureFingerprint, PriorRound, RepairDecision};
use super::runner::{read_repository_policy, RunnerEnv, VerificationRunner};
use super::snapshot::{EphemeralWorkspace, SourceSnapshot};
use super::store::{settled_payload, VerificationStore};
use super::VerificationActivation;

/// How an attempt to give a pass's lease back ended.
///
/// Three outcomes, not two: "superseded" is neither success nor a store
/// failure, and collapsing it into either one loses the distinction an
/// operator needs. See [`VerificationController::release_lease`].
enum LeaseReturn {
    /// The lease was ours and is now cleared.
    Returned,
    /// There was no lease to return.
    NotHeld,
    /// A later pass took the gate; its lease is not ours to clear.
    Superseded { holder: super::ids::Generation },
}

/// Everything the controller needs that it does not own.
pub struct ControllerDeps {
    /// Behind an `Arc` because every store call has to be handed to
    /// `spawn_blocking`, which needs `'static` ownership — see
    /// `VerificationController::on_store`.
    pub store: Arc<VerificationStore>,
    pub sink: Arc<dyn VerificationEventSink>,
    pub activation: VerificationActivation,
    /// Identifies this worker for lease ownership.
    pub worker_id: String,
    /// Where ephemeral checkouts are materialised. On this machine that should
    /// be the SSD, not the root volume.
    pub workspace_root: std::path::PathBuf,
}

/// One pass over one gate.
pub struct VerificationController {
    deps: ControllerDeps,
    /// Event sequence counter, seeded from the journal once per pass.
    ///
    /// Deriving each event's sequence from `journal.last_sequence()` would
    /// `read_dir` the gate's journal directory per event — and one event is
    /// emitted per command, so a policy with N checks paid N directory scans
    /// purely to number its own events.
    event_sequence: std::sync::atomic::AtomicU64,
}

/// What a pass concluded.
#[derive(Debug, Clone, PartialEq)]
pub enum PassOutcome {
    Verified,
    Unverified,
    /// Diagnostics are ready for the owning engineer.
    Repairing(Box<repair::RepairRequest>),
    Exhausted {
        reason: String,
    },
    Unavailable {
        reason: String,
    },
    Cancelled,
    /// The attempt could not be completed; the outbox entry remains so another
    /// pass will pick it up.
    RetryLater {
        reason: String,
    },
}

impl VerificationController {
    pub fn new(deps: ControllerDeps) -> Self {
        Self {
            deps,
            event_sequence: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn emit(
        &self,
        kind: VerificationEventKind,
        gate: &VerificationGate,
        attestation: Option<&VerificationAttestation>,
        attempt: Option<&AttemptId>,
        detail: Option<String>,
    ) {
        let sequence = self
            .event_sequence
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let event = VerificationEvent::from_gate(
            kind,
            gate,
            sequence,
            attestation.map(|a| a.attestation_id.clone()),
            attempt.cloned(),
            detail,
        );
        self.deps.sink.emit(&event);
    }

    /// Run a store call on the blocking pool.
    ///
    /// `VerificationStore` states the contract itself: "All methods are sync;
    /// file I/O happens on the caller's thread. Callers inside an async
    /// runtime should wrap in `tokio::task::spawn_blocking`." Every mutating
    /// call takes a blocking `flock` — which a stalled peer can hold for as
    /// long as it likes — and `recover` replays a whole journal. Doing it
    /// inline parked a tokio worker per verification, which stalls unrelated
    /// request handling rather than merely this gate.
    ///
    /// Centralised here rather than restated at each of a dozen call sites,
    /// because a contract restated a dozen times is a contract that gets
    /// forgotten at the thirteenth.
    async fn on_store<T, F>(&self, what: &'static str, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&VerificationStore) -> Result<T> + Send + 'static,
    {
        let store = Arc::clone(&self.deps.store);
        tokio::task::spawn_blocking(move || f(&store))
            .await
            .with_context(|| format!("verification store task panicked: {what}"))?
    }

    async fn load_gate(&self, gate_id: &GateId) -> Result<VerificationGate> {
        let id = gate_id.clone();
        self.on_store("load gate", move |store| store.load_gate(&id))
            .await
    }

    async fn claim_lease(&self, gate_id: &GateId, now: DateTime<Utc>) -> Result<VerificationGate> {
        let id = gate_id.clone();
        let worker = self.deps.worker_id.clone();
        self.on_store("claim lease", move |store| {
            store.claim_lease(&id, &worker, now, super::DEFAULT_LEASE_SECS)
        })
        .await
    }

    /// Give the gate's lease back without changing anything else.
    ///
    /// A lease belongs to a *pass*, and every way a pass can end has to return
    /// it. The five settle paths do it as part of the transition they commit;
    /// this covers the two that commit nothing — an indeterminate attempt, and
    /// a pass that ends by erroring. Leaving it held locks every other worker
    /// out for the rest of the TTL, and each pass claims under a fresh worker
    /// id, so on the repair path the successor candidate cannot be picked up
    /// at all.
    ///
    /// Best-effort by design: it commits under the same fencing generation the
    /// pass held, so a worker that has already been superseded fails the
    /// compare-and-set and correctly leaves the new holder's lease alone.
    ///
    /// **Two failures live here and they are not the same event**, which is
    /// why the outcome is classified rather than reduced to `Result`. Being
    /// superseded is the *expected* end of a long pass under the current
    /// no-renewal design — the lease lapsed, a duplicate driver claimed, and
    /// the successor's lease must be left alone. Anything else means the gate
    /// came to rest owning a lease no live worker holds, and every other
    /// worker is locked out until the TTL burns down. Logging both at `debug!`
    /// made the second invisible behind the first, which happens on nearly
    /// every real pass.
    async fn release_lease(&self, gate_id: &GateId, generation: super::ids::Generation) {
        let id = gate_id.clone();
        let released = self
            .on_store("release the pass lease", move |store| {
                let stored = store.load_gate(&id)?;
                // Read the fence *before* trying to commit, so "this pass was
                // superseded" is a distinguishable answer rather than one
                // error string among many.
                if stored.generation != generation {
                    return Ok(LeaseReturn::Superseded {
                        holder: stored.generation,
                    });
                }
                if stored.lease.is_none() {
                    return Ok(LeaseReturn::NotHeld);
                }
                let mut next = stored;
                next.lease = None;
                next.updated_at = Utc::now();
                store.commit_gate(next, generation, |gate| JournalPayload::GateUpdate { gate })?;
                Ok(LeaseReturn::Returned)
            })
            .await;
        match released {
            Ok(LeaseReturn::Returned) | Ok(LeaseReturn::NotHeld) => {},
            Ok(LeaseReturn::Superseded { holder }) => {
                // Not an error, and not silent either: this is the moment a
                // finished pass learns its work is going to be thrown away by
                // the generation fence, and an operator correlating a
                // discarded 16-minute green result needs to see it.
                tracing::info!(
                    gate_id = %gate_id,
                    held_generation = %generation,
                    current_generation = %holder,
                    "[VERIFICATION] this pass was superseded before it could return its \
                     lease; the successor owns the gate and this pass's result is discarded"
                );
            },
            Err(error) => {
                // Still holding the generation, so the lease really is ours
                // and really did not go back. The gate is now locked to a
                // worker that has finished, for the rest of the TTL.
                tracing::warn!(
                    gate_id = %gate_id,
                    %error,
                    "[VERIFICATION] the pass lease could not be returned; the gate stays \
                     locked to a finished worker until its TTL expires"
                );
            },
        }
    }

    /// Hand back a pass result, returning the lease first if the pass errored.
    async fn released_on_error<T>(
        &self,
        gate_id: &GateId,
        generation: super::ids::Generation,
        result: Result<T>,
    ) -> Result<T> {
        if result.is_err() {
            self.release_lease(gate_id, generation).await;
        }
        result
    }

    /// Run one verification pass for `gate_id`.
    ///
    /// `source_root` is the real workspace the candidate was applied to;
    /// nothing is executed there. `baseline` is the pinned owner/project
    /// policy — see `policy::resolve` for why it is passed rather than read
    /// from the repository at this moment.
    pub async fn run_pass(
        &self,
        gate_id: &GateId,
        source_root: &std::path::Path,
        baseline: Option<&VerificationPolicy>,
    ) -> Result<PassOutcome> {
        if !self.deps.activation.records_evidence() {
            return Ok(PassOutcome::RetryLater {
                reason: "verification controller is disabled".into(),
            });
        }

        let now = Utc::now();

        // Read status BEFORE claiming. `claim_lease` bumps the fencing
        // generation and commits a journal transaction, so polling an already
        // settled gate would grow its journal without bound — and a terminal
        // gate whose outbox entry was never retired gets polled on every
        // scheduler tick, which is exactly the `blocked_partial` case.
        let existing = self.load_gate(gate_id).await?;

        if existing.status.is_terminal() {
            // Retire the work item so the scheduler stops re-offering it.
            // This is projection repair, not a state change — the gate is
            // already where it is going to stay.
            let id = gate_id.clone();
            self.on_store("retire a settled gate's work item", move |store| {
                store.retire_outbox_for_gate(&id)
            })
            .await?;
            return Ok(match existing.status {
                GateStatus::Cancelled => PassOutcome::Cancelled,
                GateStatus::Verified => PassOutcome::Verified,
                GateStatus::Unverified => PassOutcome::Unverified,
                // `Unavailable` is not terminal, so it never lands here — it
                // falls through to a fresh attempt below.
                other => PassOutcome::Exhausted {
                    reason: existing
                        .terminal_reason
                        .unwrap_or_else(|| format!("gate settled as {other:?}")),
                },
            });
        }

        // A gate mid-repair has no candidate to verify: the previous one was
        // invalidated when repair started and the successor does not exist
        // yet. Running here would verify the pre-repair tree.
        //
        // The wait still has to be bounded. A repair that never produces a
        // successor — the engineer failed, or was never dispatched — would
        // otherwise hold its task open forever, because returning early skips
        // every budget check further down. This is the one place the elapsed
        // ceiling can be applied to a gate that is not running anything.
        if existing.status == GateStatus::Repairing {
            if let super::budgets::BudgetVerdict::Exhausted { reason, .. } =
                super::budgets::may_start_pass(&existing, now)
            {
                let gate = self
                    .claim_lease(gate_id, now)
                    .await
                    .context("claim lease to expire a stalled repair")?;
                let generation = gate.generation;
                let settled = self
                    .settle_exhausted(
                        &gate,
                        generation,
                        format!("repair never produced a successor candidate: {reason}"),
                        // No attempt ran here — the gate is being settled for
                        // what did *not* happen — so the last round's
                        // attestation stays the one the gate points at.
                        None,
                    )
                    .await;
                return self.released_on_error(gate_id, generation, settled).await;
            }
            return Ok(PassOutcome::RetryLater {
                reason: "gate is repairing; awaiting the successor candidate".into(),
            });
        }

        let gate = self
            .claim_lease(gate_id, now)
            .await
            .context("claim verification lease")?;
        let generation = gate.generation;

        // Everything from here holds the lease, so every exit has to give it
        // back. The settle paths clear it as part of their own transition; the
        // wrapper covers the ways out that commit nothing at all — a panicking
        // blocking task, a runner error, a refused compare-and-set.
        //
        // The renewal task keeps the lease live for exactly as long as the
        // pass runs — a real pass takes minutes against a 120-second TTL, and
        // an expired lease is an invitation for a duplicate driver to claim
        // the gate and discard this pass's finished result at the fence.
        let renewal = self.spawn_lease_renewal(gate_id.clone(), generation);
        let outcome = self
            .run_leased_pass(&gate, generation, source_root, baseline, now)
            .await;
        renewal.abort();
        self.released_on_error(gate_id, generation, outcome).await
    }

    /// Keep a pass's lease alive until the pass returns.
    ///
    /// Renews at half the TTL so a single missed tick is survivable, and
    /// stops on the first refusal: a refused renewal means the lease was
    /// claimed out from under this pass or the gate settled, and either way
    /// the generation fence — not the lease — decides what this pass's
    /// result is worth. Renewals are deliberately not journaled
    /// (see `VerificationStore::renew_lease`), so a crashed renewer costs
    /// nothing but the lease expiring, which is the same outcome as the
    /// worker having died.
    ///
    /// The caller aborts the returned handle the moment the pass returns; an
    /// aborted renewal mid-`spawn_blocking` finishes that one write harmlessly
    /// — it only pushes `expires_at` out on a lease the pass still held.
    pub(super) fn spawn_lease_renewal(
        &self,
        gate_id: GateId,
        generation: super::ids::Generation,
    ) -> tokio::task::JoinHandle<()> {
        let store = Arc::clone(&self.deps.store);
        let worker = self.deps.worker_id.clone();
        tokio::spawn(async move {
            let period =
                std::time::Duration::from_secs((super::DEFAULT_LEASE_SECS.max(2) as u64) / 2);
            let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let store = Arc::clone(&store);
                let holder = worker.clone();
                let id = gate_id.clone();
                let renewed = tokio::task::spawn_blocking(move || {
                    store.renew_lease(
                        &id,
                        &holder,
                        generation,
                        Utc::now(),
                        super::DEFAULT_LEASE_SECS,
                    )
                })
                .await;
                match renewed {
                    Ok(Ok(_)) => {},
                    Ok(Err(error)) => {
                        tracing::info!(
                            gate_id = %gate_id,
                            %error,
                            "[VERIFICATION] lease renewal stopped; the generation fence decides \
                             whether this pass's result still lands"
                        );
                        break;
                    },
                    Err(error) => {
                        tracing::warn!(
                            gate_id = %gate_id,
                            %error,
                            "[VERIFICATION] lease renewal task panicked; the lease will lapse \
                             on its own TTL"
                        );
                        break;
                    },
                }
            }
        })
    }

    /// The body of a pass, from the moment the lease is held.
    ///
    /// Split out so that "a pass that ends by erroring must not keep the
    /// lease" is one rule in one place rather than a `?` audit that has to be
    /// repeated every time a fallible call is added.
    async fn run_leased_pass(
        &self,
        gate: &VerificationGate,
        generation: super::ids::Generation,
        source_root: &std::path::Path,
        baseline: Option<&VerificationPolicy>,
        now: DateTime<Utc>,
    ) -> Result<PassOutcome> {
        // Seed the event sequence from the journal once, here, rather than
        // per event. Events stay monotonic and gap-detectable for a consumer
        // without the walk.
        let id = gate.gate_id.clone();
        let seed = self
            .on_store("read the journal's last sequence", move |store| {
                Ok(store
                    .journal()
                    .last_sequence(&id)
                    .ok()
                    .flatten()
                    .map_or(0, |s| s.saturating_add(1)))
            })
            .await
            .unwrap_or(0);
        self.event_sequence
            .store(seed, std::sync::atomic::Ordering::Relaxed);

        // (Terminal and repairing states were handled before the lease claim.)

        // Before any expensive work. A gate whose elapsed or spend ceiling has
        // already passed must not start a fresh test run just because a repair
        // round remains — it would burn the machine for a result the gate can
        // no longer act on.
        if let super::budgets::BudgetVerdict::Exhausted { reason, .. } =
            super::budgets::may_start_pass(gate, now)
        {
            // Same shape: the budget stopped this pass before it ran, so there
            // is no new evidence and the existing reference stands.
            return self.settle_exhausted(gate, generation, reason, None).await;
        }

        self.emit(VerificationEventKind::Started, gate, None, None, None);

        // ---- policy ------------------------------------------------------
        // A policy we cannot read is `unavailable`, never "no checks
        // configured". One means "there was nothing to run"; the other means
        // "we could not run it", and collapsing them turns an outage into a
        // clean bill of health.
        let repository = match read_repository_policy(source_root) {
            Ok(found) => found,
            Err(error) => {
                return self
                    .settle_unavailable(gate, generation, format!("policy unreadable: {error}"))
                    .await
            },
        };
        let resolved = match resolve(baseline, repository.as_ref(), &[]) {
            Ok(resolved) => resolved,
            Err(error) => {
                return self
                    .settle_unavailable(
                        gate,
                        generation,
                        format!("policy could not be resolved: {error}"),
                    )
                    .await
            },
        };

        if !resolved.has_required_checks() {
            return self.settle_unverified(gate, generation).await;
        }

        // ---- snapshot ----------------------------------------------------
        // Hashing a source tree is multi-second, CPU- and IO-bound and
        // entirely synchronous; copying one is worse. Running either inline
        // would park a tokio worker for the duration — with a few concurrent
        // verifications that stalls unrelated request handling, not just this
        // gate. Both go to the blocking pool.
        //
        // They are two hops rather than one on purpose. The attestation key is
        // computable from the *capture* alone, and a key that already has a
        // green attestation must not pay for a full-tree copy to discover
        // that. The copy is the expensive half and it now happens only on a
        // cache miss.
        let src = source_root.to_path_buf();
        let captured = tokio::task::spawn_blocking(move || {
            let snapshot = SourceSnapshot::capture(&src, &[])?;
            let digest = snapshot.digest();
            Ok::<_, anyhow::Error>((snapshot, digest))
        })
        .await
        .context("verification snapshot task panicked")?;

        let (snapshot, snapshot_digest) = match captured {
            Ok(pair) => pair,
            Err(error) => {
                return self
                    .settle_unavailable(
                        gate,
                        generation,
                        format!("source snapshot failed: {error}"),
                    )
                    .await
            },
        };

        // ---- attestation --------------------------------------------------
        // The toolchain is probed against the *source* tree rather than the
        // materialised one. They are the same tree by construction — the
        // workspace is a copy of this very snapshot, `rust-toolchain.toml` and
        // its equivalents included — and probing here is what lets the whole
        // key exist before anything is copied.
        let runner = VerificationRunner::new(RunnerEnv::new(
            super::runner::probe_toolchain(source_root).await,
        ));
        let key = AttestationKey {
            scope: self.deps.store.scope().clone(),
            project_binding: gate.project_binding.clone(),
            candidate_ref: gate.current_candidate.candidate_ref.clone(),
            proposal_ref: Some(gate.current_candidate.candidate_ref.clone()),
            snapshot_digest: snapshot_digest.clone(),
            policy_digest: resolved.digest(),
            runner_env_digest: runner.env().digest(),
        };

        // Reuse only on an exact whole-key match, and only when the prior
        // attestation actually settled green. A sealed red attestation is
        // evidence of failure, not a cache hit.
        let lookup_key = key.clone();
        let reusable = self
            .on_store("look up a reusable attestation", move |store| {
                store.find_reusable(&lookup_key)
            })
            .await;
        if let Ok(Some(existing)) = reusable {
            if let Some(accepted) = &existing.accepted_result {
                if accepted.outcome == AttemptOutcome::Green {
                    return self.settle_verified(gate, generation, &existing).await;
                }
            }
        }

        // ---- workspace ------------------------------------------------------
        let workspace_path = self.deps.workspace_root.join(workspace_dir_name(gate));
        let src = source_root.to_path_buf();
        let writable_roots = resolved.sandbox.writable_roots.clone();
        let workspace_root = self.deps.workspace_root.clone();

        let materialised = tokio::task::spawn_blocking(move || {
            // Reap before materialising, in the same hop. Cleanup is
            // `Drop`-only, and the failures that strand a full repository copy
            // — SIGKILL, OOM, power loss — run no destructor. A live pass
            // elsewhere is safe: reaping only removes a directory whose lease
            // it can take exclusively, and a live pass holds that lease for
            // its whole lifetime.
            for stale in super::snapshot::reap_stale_workspaces(&workspace_root) {
                tracing::info!(
                    path = %stale.display(),
                    "[VERIFICATION] reaped a stranded verification workspace"
                );
            }
            EphemeralWorkspace::materialise(snapshot, &src, workspace_path, writable_roots)
        })
        .await
        .context("verification workspace task panicked")?;

        let workspace = match materialised {
            Ok(workspace) => workspace,
            Err(error) => {
                return self
                    .settle_unavailable(
                        gate,
                        generation,
                        format!("workspace materialisation failed: {error}"),
                    )
                    .await
            },
        };

        // Everything from here to the run's end is fallible *and* holds the
        // workspace, so it is grouped: an early `?` would drop a materialised
        // repository copy — a recursive delete plus a lock release — inline on
        // this tokio worker, which is exactly what the mutation-scan hop below
        // exists to prevent. One error path, one off-runtime teardown.
        let prepared = async {
            let attestation = VerificationAttestation::new(key)?;
            let attestation = self
                .on_store("create attestation", move |store| {
                    store.create_attestation(&attestation)
                })
                .await
                .context("create attestation")?;

            // ---- run ------------------------------------------------------
            let attempt_id = AttemptId::new();
            let started_at = Utc::now();
            let report = runner.run(&resolved, workspace.path()).await?;
            Ok::<_, anyhow::Error>((attestation, attempt_id, started_at, report))
        }
        .await;

        let (attestation, attempt_id, started_at, report) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                Self::discard_workspace(workspace).await;
                return Err(error);
            },
        };

        for result in &report.results {
            self.emit(
                VerificationEventKind::CommandCompleted,
                gate,
                Some(&attestation),
                Some(&attempt_id),
                Some(format!(
                    "{} {}",
                    result.command,
                    if result.passed() { "passed" } else { "failed" }
                )),
            );
        }

        // A check that rewrote tracked source has invalidated its own
        // evidence: the attestation keys on an input that no longer describes
        // what ran.
        //
        // The workspace is *moved* into the blocking task and dropped there.
        // That puts both the re-hash and the recursive directory removal in
        // `Drop` on the blocking pool — deleting a materialised source tree is
        // itself seconds of synchronous IO, and it is the last thing that
        // should stall an async worker.
        let mutations = tokio::task::spawn_blocking(move || {
            let report = workspace.detect_mutations();
            drop(workspace);
            report
        })
        .await
        .context("verification mutation scan task panicked")??;
        let outcome = if mutations.invalidates_run() {
            AttemptOutcome::Indeterminate
        } else {
            report.outcome
        };

        let attempt = VerificationAttempt {
            attempt_id: attempt_id.clone(),
            generation,
            fenced_lease: gate
                .lease
                .as_ref()
                .map(|l| l.token.clone())
                .unwrap_or_default(),
            started_at,
            settled_at: Some(Utc::now()),
            command_results: report.results.clone(),
            outcome,
        };
        let attestation_id = attestation.attestation_id.clone();
        let attestation = self
            .on_store("append verification attempt", move |store| {
                store.append_attempt(&attestation_id, attempt)
            })
            .await
            .context("append verification attempt")?;

        match outcome {
            AttemptOutcome::Indeterminate => {
                let detail = if mutations.invalidates_run() {
                    format!(
                        "a check modified tracked source ({}); the run is invalidated",
                        mutations.tracked_mutations.join(", ")
                    )
                } else {
                    "attempt did not complete".to_string()
                };
                self.emit(
                    VerificationEventKind::AttemptIndeterminate,
                    gate,
                    Some(&attestation),
                    Some(&attempt_id),
                    Some(detail.clone()),
                );
                // Left non-terminal on purpose: the outbox entry survives and
                // a later pass re-runs under a new attempt.
                //
                // The *pass* is over even though the gate is not, so the lease
                // goes back. This is the one non-error exit that commits no
                // gate transition, and it was leaving the gate locked to a
                // finished worker for the rest of the TTL — which the next
                // pass, claiming under a fresh worker id, would then be
                // refused.
                self.release_lease(&gate.gate_id, generation).await;
                Ok(PassOutcome::RetryLater { reason: detail })
            },
            AttemptOutcome::Green => {
                let attestation_id = attestation.attestation_id.clone();
                let result = AcceptedResult {
                    attempt_id,
                    outcome: AttemptOutcome::Green,
                    accepted_at: Utc::now(),
                    generation,
                };
                let accepted = self
                    .on_store("accept a green result", move |store| {
                        store.accept_result(&attestation_id, result)
                    })
                    .await?;
                self.settle_verified(gate, generation, &accepted.into_inner())
                    .await
            },
            AttemptOutcome::Red => {
                let attestation_id = attestation.attestation_id.clone();
                let result = AcceptedResult {
                    attempt_id: attempt_id.clone(),
                    outcome: AttemptOutcome::Red,
                    accepted_at: Utc::now(),
                    generation,
                };
                let accepted = self
                    .on_store("accept a red result", move |store| {
                        store.accept_result(&attestation_id, result)
                    })
                    .await?;
                let attestation = accepted.into_inner();
                self.emit(
                    VerificationEventKind::ChecksFailed,
                    gate,
                    Some(&attestation),
                    Some(&attempt_id),
                    None,
                );

                // Observe records the failure and stops. Repair would dispatch
                // a new child execution onto a task that — under observe —
                // already completed, and a `repairing` gate would then wait
                // for a successor candidate nothing is going to produce. The
                // red attempt and its accepted result are the evidence observe
                // exists to collect; the gate settles on them.
                if !self.deps.activation.gates_completion() {
                    return self
                        .settle_exhausted(
                            gate,
                            generation,
                            "checks failed under observe; repair is not dispatched".to_string(),
                            // Under observe this attestation is the whole
                            // point of the run, so the gate must name it.
                            Some(&attestation),
                        )
                        .await;
                }

                let fingerprint = FailureFingerprint::from_results(&report.results);
                let prior = self.prior_round(gate).await?;
                let diagnostics = repair::format_diagnostics(&report.results);
                match repair::decide(
                    gate,
                    Utc::now(),
                    &fingerprint,
                    &snapshot_digest,
                    prior.as_ref(),
                    diagnostics,
                ) {
                    RepairDecision::Repair(request) => {
                        self.settle_repairing(gate, generation, &attestation, *request)
                            .await
                    },
                    RepairDecision::Exhausted { reason } => {
                        // The red round that spent the budget is the evidence
                        // an operator needs to read; point the gate at it.
                        self.settle_exhausted(gate, generation, reason, Some(&attestation))
                            .await
                    },
                }
            },
        }
    }

    /// Tear an ephemeral workspace down off the runtime.
    ///
    /// `EphemeralWorkspace`'s `Drop` recursively deletes a full repository
    /// copy and releases its lock file — seconds of synchronous IO. The
    /// success path already moves the workspace into a blocking task for
    /// exactly this reason; the error paths need the same treatment, because
    /// a `?` that drops it where it stands does the identical work on a tokio
    /// worker.
    ///
    /// A panicking teardown is logged, not propagated: the caller is already
    /// returning an error, and the reaper collects a directory whose lock
    /// nothing holds.
    async fn discard_workspace(workspace: EphemeralWorkspace) {
        if let Err(error) = tokio::task::spawn_blocking(move || drop(workspace)).await {
            tracing::warn!(
                %error,
                "[VERIFICATION] verification workspace teardown task panicked; the reaper \
                 will collect the directory"
            );
        }
    }

    /// The previous round's fingerprint, for no-progress detection.
    ///
    /// Reads from committed attestations rather than in-memory state, so the
    /// comparison survives a restart between rounds.
    async fn prior_round(&self, gate: &VerificationGate) -> Result<Option<PriorRound>> {
        let Some(active) = gate.active_attestation_ref.clone() else {
            return Ok(None);
        };
        let loaded = self
            .on_store("load the prior round's attestation", move |store| {
                store.load_attestation(&active)
            })
            .await;
        let Ok(prior) = loaded else {
            return Ok(None);
        };
        let Some(last) = prior.attempts.last() else {
            return Ok(None);
        };
        Ok(Some(PriorRound {
            fingerprint: FailureFingerprint::from_results(&last.command_results),
            snapshot_digest: prior.key.snapshot_digest.clone(),
        }))
    }

    async fn settle_verified(
        &self,
        gate: &VerificationGate,
        generation: super::ids::Generation,
        attestation: &VerificationAttestation,
    ) -> Result<PassOutcome> {
        let green = attestation
            .accepted_result
            .as_ref()
            .is_some_and(|a| a.outcome == AttemptOutcome::Green);

        // The compare-and-set of §4.3, checked against the *stored* gate.
        //
        // Re-reading matters: `gate` is a snapshot taken before the checks
        // ran, which in this repository can be sixteen minutes old. Asking
        // that snapshot whether its own `current_candidate` matches the one
        // we verified compares it to itself and always passes, so the
        // stale-candidate guard — the thing standing between a repaired gate
        // and a green released off the pre-repair tree — would never fire.
        let stored = self.load_gate(&gate.gate_id).await?;
        stored
            .may_finalize_green(
                &gate.current_candidate,
                &attestation.attestation_id,
                green,
                generation,
            )
            .map_err(|e| anyhow!("refusing to release candidate success: {e}"))?;

        let mut next = stored;
        next.status = GateStatus::Verified;
        // The lease belongs to the pass, and the pass is over. Holding it
        // past settlement locks every other worker out for the rest of the
        // TTL — which on the repair path means the successor candidate
        // cannot be picked up at all, because re-arming happens seconds
        // later and each pass claims under a fresh worker id.
        next.lease = None;
        next.active_attestation_ref = Some(attestation.attestation_id.clone());
        next.updated_at = Utc::now();

        let accepted = attestation.attestation_id.clone();
        let settled = self
            .on_store("commit a verified gate", move |store| {
                let retire = outbox_entry_for(store, &next.gate_id);
                store.commit_gate(next, generation, settled_payload(retire, Some(accepted)))
            })
            .await?;
        self.emit(
            VerificationEventKind::Passed,
            &settled,
            Some(attestation),
            None,
            None,
        );
        Ok(PassOutcome::Verified)
    }

    async fn settle_unverified(
        &self,
        gate: &VerificationGate,
        generation: super::ids::Generation,
    ) -> Result<PassOutcome> {
        let mut next = gate.clone();
        next.status = GateStatus::Unverified;
        // The lease belongs to the pass, and the pass is over. Holding it
        // past settlement locks every other worker out for the rest of the
        // TTL — which on the repair path means the successor candidate
        // cannot be picked up at all, because re-arming happens seconds
        // later and each pass claims under a fresh worker id.
        next.lease = None;
        next.terminal_reason = Some("no required checks are configured for this project".into());
        next.updated_at = Utc::now();
        let settled = self
            .on_store("commit an unverified gate", move |store| {
                let retire = outbox_entry_for(store, &next.gate_id);
                store.commit_gate(next, generation, settled_payload(retire, None))
            })
            .await?;
        self.emit(
            VerificationEventKind::Unverified,
            &settled,
            None,
            None,
            settled.terminal_reason.clone(),
        );
        Ok(PassOutcome::Unverified)
    }

    async fn settle_unavailable(
        &self,
        gate: &VerificationGate,
        generation: super::ids::Generation,
        reason: String,
    ) -> Result<PassOutcome> {
        let mut next = gate.clone();
        next.status = GateStatus::Unavailable;
        // The lease belongs to the pass, and the pass is over. Holding it
        // past settlement locks every other worker out for the rest of the
        // TTL — which on the repair path means the successor candidate
        // cannot be picked up at all, because re-arming happens seconds
        // later and each pass claims under a fresh worker id.
        next.lease = None;
        next.terminal_reason = Some(reason.clone());
        next.updated_at = Utc::now();
        // Keep the work item. Retiring it — which every genuinely terminal
        // settle path does — would mean no later attempt is ever offered, so
        // one unreadable policy would permanently strand the candidate.
        let settled = self
            .on_store("commit an unavailable gate", move |store| {
                store.commit_gate(next, generation, |gate| JournalPayload::GateUpdate { gate })
            })
            .await?;
        self.emit(
            VerificationEventKind::Unavailable,
            &settled,
            None,
            None,
            Some(reason.clone()),
        );
        Ok(PassOutcome::Unavailable { reason })
    }

    /// Settle a gate that will not reach green.
    ///
    /// `attestation` is the evidence this settlement rests on, when there is
    /// any: the red attempt that exhausted the repair budget, or — under
    /// `observe` — the red attempt that *is* the entire deliverable. Recording
    /// it on the gate is what makes the evidence reachable from the record a
    /// consumer actually reads; without it the attestation still exists on
    /// disk but nothing points at it, and `verification_state` reports a
    /// failure whose diagnostics can only be found by scanning the store.
    ///
    /// `None` is for the settlement that has no attempt behind it — a repair
    /// that never produced a successor candidate — and it leaves any earlier
    /// reference in place rather than clearing it.
    async fn settle_exhausted(
        &self,
        gate: &VerificationGate,
        generation: super::ids::Generation,
        reason: String,
        attestation: Option<&VerificationAttestation>,
    ) -> Result<PassOutcome> {
        let mut next = gate.clone();
        next.status = GateStatus::Exhausted;
        // The lease belongs to the pass, and the pass is over. Holding it
        // past settlement locks every other worker out for the rest of the
        // TTL — which on the repair path means the successor candidate
        // cannot be picked up at all, because re-arming happens seconds
        // later and each pass claims under a fresh worker id.
        next.lease = None;
        if let Some(attestation) = attestation {
            next.active_attestation_ref = Some(attestation.attestation_id.clone());
        }
        next.terminal_reason = Some(reason.clone());
        next.updated_at = Utc::now();
        let accepted = attestation.map(|a| a.attestation_id.clone());
        let settled = self
            .on_store("commit an exhausted gate", move |store| {
                let retire = outbox_entry_for(store, &next.gate_id);
                store.commit_gate(next, generation, settled_payload(retire, accepted))
            })
            .await?;
        self.emit(
            VerificationEventKind::Exhausted,
            &settled,
            attestation,
            None,
            Some(reason.clone()),
        );
        Ok(PassOutcome::Exhausted { reason })
    }

    async fn settle_repairing(
        &self,
        gate: &VerificationGate,
        generation: super::ids::Generation,
        attestation: &VerificationAttestation,
        request: repair::RepairRequest,
    ) -> Result<PassOutcome> {
        let mut next = gate.clone();
        next.status = GateStatus::Repairing;
        // The lease belongs to the pass, and the pass is over. Holding it
        // past settlement locks every other worker out for the rest of the
        // TTL — which on the repair path means the successor candidate
        // cannot be picked up at all, because re-arming happens seconds
        // later and each pass claims under a fresh worker id.
        next.lease = None;
        next.active_attestation_ref = Some(attestation.attestation_id.clone());
        next.spend = super::budgets::consume_round(&next.spend);
        next.updated_at = Utc::now();

        // Retire the outbox entry: the candidate it points at is invalidated
        // the moment repair starts (§4.2). Leaving it would let another worker
        // pick up the superseded candidate and verify the pre-repair tree. A
        // fresh entry is enqueued when the repaired candidate is registered.
        let settled = self
            .on_store("commit a repairing gate", move |store| {
                let retire = outbox_entry_for(store, &next.gate_id);
                store.commit_gate(next, generation, move |gate| JournalPayload::RepairRound {
                    gate,
                    retire_outbox_entry: retire,
                    enqueue: None,
                })
            })
            .await?;
        self.emit(
            VerificationEventKind::RepairStarted,
            &settled,
            Some(attestation),
            None,
            Some(format!("round {}", request.round)),
        );
        Ok(PassOutcome::Repairing(Box::new(request)))
    }

    /// Charge a repair round's cost against the gate's ledger.
    ///
    /// Called by the terminal seam once a repair execution settles, since the
    /// controller does not run the coding work itself — the owning engineer
    /// does, and only the caller knows what it cost. Charging is saturating
    /// and refuses refunds, so a provider reporting a nonsense negative cost
    /// cannot buy the gate more budget.
    ///
    /// The generation is read from the stored gate rather than taken from the
    /// caller: the caller is a child-terminal handler, not a pass, so it
    /// holds no fence of its own — and a charge is an accounting append, not
    /// a state transition a stale writer could corrupt.
    pub async fn charge_repair_cost(&self, gate_id: &GateId, usd: f64) -> Result<VerificationGate> {
        let id = gate_id.clone();
        self.on_store("charge a repair round's cost", move |store| {
            let gate = store.load_gate(&id)?;
            let generation = gate.generation;
            let mut next = gate;
            next.spend = super::budgets::charge(&next.spend, usd);
            next.updated_at = Utc::now();
            store.commit_gate(next, generation, |gate| JournalPayload::GateUpdate { gate })
        })
        .await
    }

    /// Cancel a gate, retaining its evidence.
    ///
    /// **No production caller.** Nothing in the service cancels a gate, so
    /// `cancelled` is currently a state only a test can reach.
    ///
    /// **Left sync for that reason, and for no other.** This is a settle path
    /// — it loads a gate, takes the store's blocking `flock` and commits a
    /// journal transaction — so it breaks the module's "store calls hop off
    /// the runtime" rule the moment it gains an `async` caller. Wire it
    /// through `on_store` before wiring it to anything.
    pub fn cancel(&self, gate_id: &GateId, generation: super::ids::Generation) -> Result<()> {
        let gate = self.deps.store.load_gate(gate_id)?;
        if gate.status.is_terminal() {
            return Ok(());
        }
        let mut next = gate.clone();
        next.status = GateStatus::Cancelled;
        // Cancelling ends the pass, so it returns the pass's lease like every
        // other settle path. This was the one settlement that did not, and a
        // cancelled gate came to rest still owning a lease nobody holds.
        next.lease = None;
        next.terminal_reason = Some("cancelled".into());
        next.updated_at = Utc::now();
        let retire = outbox_entry_for(&self.deps.store, &gate.gate_id);
        let settled =
            self.deps
                .store
                .commit_gate(next, generation, settled_payload(retire, None))?;
        self.emit(VerificationEventKind::Cancelled, &settled, None, None, None);
        Ok(())
    }
}

/// The directory name for one pass's ephemeral workspace.
///
/// Gate and revision are there so an operator can tell what a directory on
/// disk belongs to; the random suffix is what makes it non-colliding, and that
/// is the load-bearing part.
///
/// The old name was `{gate}-r{revision}` — deterministic, and therefore
/// terminal for the gate the first time a hard kill stranded a checkout there.
/// Cleanup is `Drop`-only, so SIGKILL, an OOM or a power cut left the directory
/// behind; every later pass then hit the same path, settled `unavailable` —
/// which *keeps* its outbox entry — and never advanced, because the candidate
/// revision only moves on red and red requires materialising. The gate could
/// never make progress again.
fn workspace_dir_name(gate: &VerificationGate) -> String {
    format!(
        "{}-r{}-{}",
        gate.gate_id,
        gate.current_candidate.revision,
        uuid::Uuid::new_v4().simple()
    )
}

/// The outbox entry to retire when settling.
///
/// Looked up rather than remembered so a settle after a restart still retires
/// the right entry.
fn outbox_entry_for(store: &VerificationStore, gate_id: &GateId) -> String {
    store
        .list_outbox()
        .ok()
        .and_then(|entries| {
            entries
                .into_iter()
                .find(|e| &e.gate_id == gate_id)
                .map(|e| e.entry_id)
        })
        .unwrap_or_default()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::super::gate::{GateBudgets, GateOrigin};
    use super::super::ids::CandidateRevision;
    use super::*;
    use crate::magician_v2::execution::file_edit::transaction::TransactionScope;

    fn gate() -> VerificationGate {
        VerificationGate::new(
            TransactionScope {
                principal: "anonymous".into(),
                workspace: "default".into(),
            },
            "proj-a",
            "task-1",
            "exec-1",
            CandidateRevision::new("ccp-1", 2).unwrap(),
            GateOrigin {
                engineer_agent_id: "engineer".into(),
                coding_profile: None,
                coding_engine: None,
                constraint_auto: false,
                coding_invocation_ref: None,
                child_execution_id: None,
            },
            GateBudgets::default(),
        )
        .unwrap()
    }

    #[test]
    fn a_workspace_name_is_never_reused_by_a_later_pass() {
        // The old name was `{gate}-r{revision}`, which two passes over one
        // candidate share. That is what made a single stranded checkout
        // terminal for the gate: every later pass hit the same directory.
        let gate = gate();
        let first = workspace_dir_name(&gate);
        let second = workspace_dir_name(&gate);
        assert_ne!(first, second);
    }

    #[test]
    fn a_workspace_name_still_says_which_gate_and_revision_it_belongs_to() {
        // Uniqueness must not cost an operator the ability to read a
        // directory listing.
        let gate = gate();
        let name = workspace_dir_name(&gate);
        assert!(name.starts_with(&format!("{}-r2-", gate.gate_id)), "{name}");
    }

    #[test]
    fn a_workspace_name_is_one_path_segment() {
        // It is joined onto the workspace root, so a separator in it would
        // write outside the directory the reaper scans.
        let name = workspace_dir_name(&gate());
        assert!(!name.contains('/') && !name.contains('\\'), "{name}");
    }
}
