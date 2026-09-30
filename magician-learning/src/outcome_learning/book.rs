//! Where a maturity tick's work comes from — the book, and the one thing that
//! starts it.
//!
//! Plan: `docs/plans/2026-08-07-opc-outcome-learning.md` §2, phase 2.
//! Doc: `docs/components/magician/outcome-learning.md`.
//!
//! [`super::worker::MaturityBook`] had no implementor and
//! [`super::worker::MaturityWorker::spawn`] had no caller, so the only producer
//! of `silent` observations had never run once. That is not a missing
//! convenience: an outcome store that only ever receives replies is
//! indistinguishable from a world where everybody answers, and a variant nobody
//! engaged with looks exactly like a variant nobody tried. This module is the
//! implementor and [`spawn_configured_maturity_sweep`] is the caller.
//!
//! # Three questions a tick needs answered, and where each is answered
//!
//! - **Which tenants?** Declared in configuration, scope by scope. Not
//!   discovered from the storage root, unlike the obligation sweep: that sweep
//!   writes reminders and this one writes observations that later become
//!   evidence, so adopting a stale, system or ephemeral-eval scope here would
//!   record judgements into a cohort nobody chose — and an append-only store
//!   cannot un-record them. Explicit ownership is the same choice
//!   [`magician::config::DeliveryHygieneConfig`] made, for the same reason.
//! - **Which acts?** Discovered, from the outward-assertions reverse index,
//!   under **every** kind of work rather than one. See the section below.
//! - **Which cohort was each act performed under?** Supplied by an
//!   [`ActCohortSource`]. The act records what went out and when; it does not
//!   record which variant version was live when it did, and inventing one would
//!   put an observation into a cohort that means nothing.
//!
//! # The axis was the whole reachability problem
//!
//! The index used to file an act under `artifact`, `engagement` and
//! `recipient` — never `program_id`, even though a run whose audience is a
//! programme sets exactly that field and nothing else. So an act performed
//! inside a programme was unreachable by work, a sweep enumerating it found
//! zero acts, and zero acts reads precisely like a programme that never said
//! anything. This book enumerates
//! [`WorkContextKind::KIND_TOKENS`](magician::magician_v2::work_context::WorkContextKind::KIND_TOKENS),
//! so a third kind of work becomes sweepable by being indexed, not by this file
//! being edited.
//!
//! # What is still unreachable, and it is counted rather than hidden
//!
//! An outward act carries `program_id` and `engagement_id` and no other work
//! field. An act bound to an [`AudienceRef::account`](magician::magician_v2::audience::AudienceRef::account),
//! a panel or a person therefore appears under **no** work axis at all, and so
//! does an act performed inside no work whatsoever. The book counts those as
//! [`MaturityScope::acts_without_work`] by comparing the work axes against
//! [`ARTIFACT_AXIS`], which every act is filed under unconditionally. It is a
//! count rather than a silence because *"this scope has no outward acts"* and
//! *"this scope's outward acts cannot be attributed to any work"* are the same
//! zero otherwise, and only one of them is a healthy system.
//!
//! # An act nobody declared a cohort for is never swept
//!
//! [`DeclaredPayloadVariants`] answers `None` for a payload no declaration
//! names, and the book counts that act as [`MaturityScope::acts_unbound`]
//! instead of binding it to a default. A defaulted cohort key would be worse
//! than no observation at all: the observation would be recorded, would be
//! comparable to nothing, and could not be withdrawn.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use magician::config::{CohortVariantConfig, OutcomeMaturityConfig};
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::evidence::outward_assertions::{
    OutwardActDisclosure, OutwardAssertionStore, OutwardScope, ARTIFACT_AXIS,
};
use magician::magician_v2::work_context::WorkContextKind;

use super::sweep::ActCohortBinding;
use super::types::Confounder;
use super::worker::{MaturityBook, MaturityScope, MaturitySweepConfig, MaturityWorker};

const LOG_TARGET: &str = "outcome_learning::maturity_book";

/// The character every derived id in this subsystem joins its fields with.
///
/// Checked here, at the boundary a declaration enters through, and not only in
/// the store: a variant ref carrying it can move the boundary between two
/// components of an observation id, so two different outcomes derive one record
/// and the second resumes the first instead of entering the sample.
const FIELD_SEP: char = '\u{1f}';

/// Who knows which variant version an act was performed under.
///
/// A trait rather than a concrete lookup because the answer is the **deciding**
/// subsystem's fact. Whoever chose the wording, the template or the framing
/// knows which version was live; the carrier that recorded the act does not, and
/// a book that guessed would fill a cohort with acts that were never comparable.
///
/// Answering `None` is the honest answer for an act nobody has declared a cohort
/// for, and the book treats it as *"not swept, and counted"* rather than as
/// *"nothing to wait for"*.
pub trait ActCohortSource: Send + Sync {
    /// The cohort this act belongs to, or `None` if nothing declares one.
    fn bind(&self, act: &OutwardActDisclosure) -> Option<ActCohortBinding>;
}

/// Cohorts declared against the **exact payload** an act carried.
///
/// The payload reference is the one durable thing an act records that a person
/// can also name: *"this exact artifact revision is version 3 of the outreach
/// wording"* is a fact an operator holds and the record does not. Binding on it
/// keeps the cohort key a stated human judgement — which is what §2 of the plan
/// requires, since there is no deterministic fact available — while keeping it
/// out of the act, which must stay a record of what happened rather than of what
/// we meant by it.
///
/// Nothing here names a domain. A support macro, a supplier chaser and a pitch
/// declare the same three fields.
#[derive(Debug, Clone, Default)]
pub struct DeclaredPayloadVariants {
    by_payload: BTreeMap<String, DeclaredCohort>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeclaredCohort {
    variant_ref: String,
    variant_version: String,
    confounders: Vec<Confounder>,
}

impl DeclaredPayloadVariants {
    /// Build the lookup, or refuse the declaration.
    ///
    /// # Every refusal, and what it costs to skip it
    ///
    /// - **A blank variant ref or version.** The version is the cohort key; a
    ///   blank one makes the sample able only to agree with itself, which is the
    ///   difference between learning and self-confirmation.
    /// - **U+001F in either.** It is what joins an observation id's components,
    ///   so a value carrying it could fold two different outcomes into one
    ///   record.
    /// - **A blank payload.** It would match no act and quietly declare nothing.
    /// - **The same payload declared twice with a different cohort.** An
    ///   identical repeat is one declaration and resumes; a changed one would put
    ///   one act's outcome in two cohorts and make every later comparison agree
    ///   with whichever copy was read last.
    pub fn new(declarations: &[CohortVariantConfig]) -> Result<Self> {
        let mut by_payload: BTreeMap<String, DeclaredCohort> = BTreeMap::new();
        for declaration in declarations {
            let variant_ref = declaration.variant_ref.trim();
            let variant_version = declaration.variant_version.trim();
            guard_cohort_component("a variant ref", variant_ref)?;
            guard_cohort_component("a variant version", variant_version)?;
            if declaration.payloads.is_empty() {
                anyhow::bail!(
                    "variant `{variant_ref}` version `{variant_version}` names no payload, so it \
                     declares a cohort no act can ever join"
                );
            }
            let cohort = DeclaredCohort {
                variant_ref: variant_ref.to_string(),
                variant_version: variant_version.to_string(),
                confounders: declaration
                    .confounders
                    .iter()
                    .map(|held| Confounder::new(held.kind.clone(), held.value.clone()))
                    .collect(),
            };
            for payload in &declaration.payloads {
                let payload = payload.trim();
                if payload.is_empty() {
                    anyhow::bail!(
                        "variant `{variant_ref}` version `{variant_version}` declares a blank \
                         payload reference: it matches no act, so the declaration would read as \
                         made while binding nothing"
                    );
                }
                match by_payload.get(payload) {
                    // An identical replay resumes.
                    Some(held) if *held == cohort => continue,
                    Some(held) => anyhow::bail!(
                        "payload `{payload}` is declared as `{}`/`{}` and also as \
                         `{variant_ref}`/`{variant_version}`: one act's outcome cannot be in two \
                         cohorts, and resolving it to whichever was read last would make every \
                         later comparison agree with the file order",
                        held.variant_ref,
                        held.variant_version
                    ),
                    None => {
                        by_payload.insert(payload.to_string(), cohort.clone());
                    },
                }
            }
        }
        Ok(Self { by_payload })
    }

    /// How many payloads carry a declared cohort.
    ///
    /// A count, so a caller can tell an empty declaration from a populated one
    /// without inferring it from a sweep that recorded nothing.
    pub fn declared_payloads(&self) -> usize {
        self.by_payload.len()
    }
}

impl ActCohortSource for DeclaredPayloadVariants {
    fn bind(&self, act: &OutwardActDisclosure) -> Option<ActCohortBinding> {
        let cohort = self.by_payload.get(act.exact_payload_artifact_ref.trim())?;
        Some(
            ActCohortBinding::new(
                act.outward_act_ref.clone(),
                cohort.variant_ref.clone(),
                cohort.variant_version.clone(),
            )
            .with_confounders(cohort.confounders.clone()),
        )
    }
}

fn guard_cohort_component(what: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        anyhow::bail!(
            "{what} must be named: a blank cohort component makes an observation comparable to \
             nothing, so the sample can only ever agree with itself"
        );
    }
    if value.contains(FIELD_SEP) {
        anyhow::bail!(
            "{what} must not contain U+001F: it is the separator that joins an observation id's \
             components, and a value carrying it could fold two different outcomes into one \
             record"
        );
    }
    Ok(())
}

/// One tenant this book sweeps, and who supplies its cohort keys.
#[derive(Clone)]
pub struct BookScope {
    pub principal: String,
    pub workspace: String,
    pub cohorts: Arc<dyn ActCohortSource>,
}

/// A [`MaturityBook`] over the outward-assertions reverse index.
#[derive(Clone)]
pub struct WorkspaceMaturityBook {
    workspace_layout: ArtifactV2Workspace,
    scopes: Vec<BookScope>,
}

impl WorkspaceMaturityBook {
    /// Build a book over explicitly named scopes.
    ///
    /// Refuses an empty roster. A book that named no scope would report every
    /// tick as having found nothing, which the worker reads as `degraded` — but
    /// only after it has been started, and only where somebody is watching the
    /// health snapshot. Refusing at construction puts the failure where it can
    /// still be read as a configuration error.
    pub fn new(workspace_layout: ArtifactV2Workspace, scopes: Vec<BookScope>) -> Result<Self> {
        if scopes.is_empty() {
            anyhow::bail!(
                "a maturity book with no scopes would sweep nothing on every tick; name the \
                 scopes whose outward acts should mature rather than starting a loop over \
                 nothing"
            );
        }
        Ok(Self {
            workspace_layout,
            scopes,
        })
    }

    /// Build a book from configuration.
    pub fn from_config(
        workspace_layout: ArtifactV2Workspace,
        config: &OutcomeMaturityConfig,
    ) -> Result<Self> {
        let mut scopes = Vec::new();
        for declared in &config.scopes {
            if declared.principal.trim().is_empty() || declared.workspace.trim().is_empty() {
                anyhow::bail!(
                    "a maturity scope must name both a principal and a workspace; a blank half \
                     addresses a path nobody owns"
                );
            }
            let cohorts = DeclaredPayloadVariants::new(&declared.variants).with_context(|| {
                format!(
                    "reading the cohort declarations for `{}/{}`",
                    declared.principal, declared.workspace
                )
            })?;
            scopes.push(BookScope {
                principal: declared.principal.trim().to_string(),
                workspace: declared.workspace.trim().to_string(),
                cohorts: Arc::new(cohorts),
            });
        }
        Self::new(workspace_layout, scopes)
    }

    /// The blocking half: read the index, load each act, bind what is declared.
    ///
    /// Every failure propagates. An unreadable index is **not** an empty one,
    /// and the worker reports the difference — a book that could not be read
    /// must never present itself as a scope with nothing to do.
    fn enumerate(&self) -> Result<Vec<MaturityScope>> {
        let store = OutwardAssertionStore::new(self.workspace_layout.clone());
        let mut out = Vec::new();
        for scope in &self.scopes {
            let outward_scope =
                OutwardScope::new(scope.principal.to_string(), scope.workspace.to_string());

            // Every kind of work, read from the carrier's own token list, so a
            // kind added later is swept the day its acts are indexed rather
            // than the day somebody notices this loop only knew about two.
            let mut seen: HashSet<String> = HashSet::new();
            let mut act_refs: Vec<String> = Vec::new();
            for axis in WorkContextKind::KIND_TOKENS {
                let found = store
                    .act_refs_under_axis(&outward_scope, axis)
                    .with_context(|| {
                        format!(
                            "listing the outward acts filed under the `{axis}` axis for `{}/{}`",
                            scope.principal, scope.workspace
                        )
                    })?;
                for act_ref in found {
                    if seen.insert(act_ref.clone()) {
                        act_refs.push(act_ref);
                    }
                }
            }

            // What the work axes could not see. Every act is filed under the
            // artifact axis whatever else is known about it, so the difference
            // is exactly the acts no kind of work names — an account, a panel
            // or a person audience, or no work at all.
            let all_acts = store
                .act_refs_under_axis(&outward_scope, ARTIFACT_AXIS)
                .with_context(|| {
                    format!(
                        "counting every outward act in `{}/{}`",
                        scope.principal, scope.workspace
                    )
                })?;
            let acts_without_work = all_acts.len().saturating_sub(act_refs.len());

            let mut bindings = Vec::new();
            let mut acts_unbound = 0usize;
            for act_ref in &act_refs {
                let Some(act) = store
                    .load_act(&outward_scope, act_ref)
                    .with_context(|| format!("loading outward act `{act_ref}` for the book"))?
                else {
                    // The index names an act the store cannot produce. Counted,
                    // never bound: recording "nobody replied" about an act we
                    // cannot read is a claim with no basis.
                    acts_unbound += 1;
                    continue;
                };
                match scope.cohorts.bind(&act) {
                    Some(binding) => bindings.push(binding),
                    None => acts_unbound += 1,
                }
            }

            if acts_unbound > 0 {
                warn!(
                    target: LOG_TARGET,
                    principal = %scope.principal,
                    workspace = %scope.workspace,
                    acts_unbound,
                    bound = bindings.len(),
                    "outward acts were found that no cohort declaration describes; they are not \
                     swept"
                );
            }

            out.push(MaturityScope {
                principal: scope.principal.to_string(),
                workspace: scope.workspace.to_string(),
                bindings,
                acts_unbound,
                acts_without_work,
            });
        }
        Ok(out)
    }
}

#[async_trait]
impl MaturityBook for WorkspaceMaturityBook {
    async fn scopes(&self) -> Result<Vec<MaturityScope>> {
        let book = self.clone();
        // The enumeration reads a directory per axis and a log per act, which is
        // filesystem work and does not belong on the reactor.
        tokio::task::spawn_blocking(move || book.enumerate())
            .await
            .context("the maturity book's enumeration did not complete")?
    }
}

/// **The named entry point.** Build the book from configuration and start the
/// sweep, or decline visibly.
///
/// Wired from `magician-bin/src/main.rs` beside the delivery-hygiene and
/// obligation sweeps. It always returns a worker: a configuration that cannot
/// be honoured produces one that never started and says why through its health
/// snapshot, because a boot path that returned an error here would either take
/// the process down over a sweep or — worse — be discarded with `let _ =` and
/// leave the failure invisible.
pub fn spawn_configured_maturity_sweep(
    workspace_layout: ArtifactV2Workspace,
    config: OutcomeMaturityConfig,
    cancel: CancellationToken,
) -> MaturityWorker {
    let held = config.sweep_config();
    let sweep_config = MaturitySweepConfig {
        enabled: held.enabled,
        paused: held.paused,
        tick_interval_secs: held.tick_interval_secs,
        default_window_days: held.default_window_days,
        variant_window_days: held.variant_window_days,
    };
    // Answered before the book is built, because an off sweep legitimately
    // names no scope and building one would refuse the empty roster — which
    // would report "misconfigured" for a switch somebody deliberately left off.
    if !sweep_config.enabled || sweep_config.paused {
        info!(
            target: LOG_TARGET,
            enabled = sweep_config.enabled,
            paused = sweep_config.paused,
            "maturity sweep not started by configuration"
        );
        return MaturityWorker::not_started(&sweep_config, cancel);
    }

    let book = match WorkspaceMaturityBook::from_config(workspace_layout.clone(), &config) {
        Ok(book) => book,
        Err(error) => {
            warn!(
                target: LOG_TARGET,
                %error,
                "the maturity sweep is enabled but its book cannot be built; nothing was started"
            );
            return MaturityWorker::declined(
                &sweep_config,
                cancel,
                format!(
                    "the maturity sweep is enabled but its book cannot be built, so nothing was \
                     started: {error}"
                ),
            );
        },
    };
    MaturityWorker::spawn(workspace_layout, Arc::new(book), sweep_config, cancel)
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeZone, Utc};

    use magician::config::OutcomeMaturityScopeConfig;
    use magician::magician_v2::evidence::outward_assertions::{OutwardChannel, PrepareOutwardAct};

    use super::super::maturity::MaturityPolicy;
    use super::super::store::{OutcomeScope, OutcomeStore};
    use super::super::sweep::run_maturity_sweep;
    use super::super::types::OutcomeLabel;
    use super::*;

    fn at(day: u32, hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, day, hour, 0, 0).unwrap()
    }

    struct Bench {
        _dir: tempfile::TempDir,
        layout: ArtifactV2Workspace,
        store: OutwardAssertionStore,
        scope: OutwardScope,
    }

    fn bench() -> Bench {
        let dir = tempfile::tempdir().expect("temp dir");
        let layout = ArtifactV2Workspace::new(dir.path());
        Bench {
            _dir: dir,
            layout: layout.clone(),
            store: OutwardAssertionStore::new(layout),
            scope: OutwardScope::new("owner", "work"),
        }
    }

    /// Prepare an act inside the work the caller names, and drive it to
    /// delivered so the feeder treats it as awaiting an outcome.
    fn delivered_act(
        bench: &Bench,
        key: &str,
        program_id: Option<&str>,
        engagement_id: Option<&str>,
        payload: &str,
    ) -> String {
        let act = bench
            .store
            .prepare(
                &bench.scope,
                &PrepareOutwardAct {
                    idempotency_key: key.to_string(),
                    program_id: program_id.map(str::to_string),
                    engagement_id: engagement_id.map(str::to_string),
                    exact_payload_artifact_ref: payload.to_string(),
                    effective_sender: "sender@example.test".to_string(),
                    intended_audience: vec!["reader@example.test".to_string()],
                    channel: OutwardChannel::Email,
                    consequence_class: "bounded_communication".to_string(),
                },
                &at(1, 0).to_rfc3339(),
            )
            .expect("prepare");
        bench
            .store
            .mark_dispatching(&bench.scope, &act.outward_act_ref, &at(1, 0).to_rfc3339())
            .expect("dispatching");
        bench
            .store
            .record_delivered(&bench.scope, &act.outward_act_ref, &at(1, 1).to_rfc3339())
            .expect("delivered");
        act.outward_act_ref
    }

    fn declaration(variant: &str, version: &str, payloads: &[&str]) -> CohortVariantConfig {
        CohortVariantConfig {
            variant_ref: variant.to_string(),
            variant_version: version.to_string(),
            payloads: payloads.iter().map(|held| held.to_string()).collect(),
            confounders: Vec::new(),
        }
    }

    fn book_over(bench: &Bench, declarations: Vec<CohortVariantConfig>) -> WorkspaceMaturityBook {
        WorkspaceMaturityBook::new(
            bench.layout.clone(),
            vec![BookScope {
                principal: bench.scope.principal.to_string(),
                workspace: bench.scope.workspace.to_string(),
                cohorts: Arc::new(
                    DeclaredPayloadVariants::new(&declarations).expect("declarations"),
                ),
            }],
        )
        .expect("book")
    }

    /// The book finds acts under **every** kind of work, not one.
    ///
    /// This is the reachability failure the whole tier turns on. The index
    /// filed `engagement_id` and never `program_id`, and the lookup read a
    /// hardcoded engagement axis — so an act performed inside a programme was
    /// unreachable, the book enumerated zero, and zero acts is exactly what a
    /// programme that never said anything looks like. A sweep over that answer
    /// reports a clean pass having considered nothing.
    #[test]
    fn the_book_enumerates_acts_under_every_kind_of_work() {
        let bench = bench();
        let engaged = delivered_act(&bench, "a", None, Some("eng-1"), "payload-a@1");
        let programmed = delivered_act(&bench, "b", Some("prog-1"), None, "payload-b@1");

        let book = book_over(
            &bench,
            vec![declaration(
                "outreach",
                "v1",
                &["payload-a@1", "payload-b@1"],
            )],
        );
        let scopes = book.enumerate().expect("enumerate");
        assert_eq!(scopes.len(), 1);

        let mut bound: Vec<String> = scopes[0]
            .bindings
            .iter()
            .map(|held| held.act_ref.clone())
            .collect();
        bound.sort();
        let mut expected = vec![engaged, programmed];
        expected.sort();
        assert_eq!(bound, expected);
        assert_eq!(scopes[0].acts_unbound, 0);
        assert_eq!(scopes[0].acts_without_work, 0);
    }

    /// An act nobody declared a cohort for is counted, never bound to a
    /// default.
    ///
    /// A defaulted cohort key would be worse than no observation: the
    /// observation would be recorded into an append-only store, would be
    /// comparable to nothing, and could not be withdrawn. The count is what
    /// keeps *"nothing outstanding"* from reading the same as *"nobody has said
    /// what any of this is"*.
    #[test]
    fn an_undeclared_payload_is_counted_unbound_rather_than_given_a_default_cohort() {
        let bench = bench();
        let declared = delivered_act(&bench, "a", None, Some("eng-1"), "payload-a@1");
        delivered_act(&bench, "b", None, Some("eng-1"), "payload-undeclared@9");

        let book = book_over(
            &bench,
            vec![declaration("outreach", "v1", &["payload-a@1"])],
        );
        let scopes = book.enumerate().expect("enumerate");
        assert_eq!(scopes[0].bindings.len(), 1);
        assert_eq!(scopes[0].bindings[0].act_ref, declared);
        assert_eq!(scopes[0].bindings[0].variant_version, "v1");
        assert_eq!(scopes[0].acts_unbound, 1);
    }

    /// An act no kind of work names is counted, so the gap is a number rather
    /// than a silence.
    ///
    /// An act bound to an account, a panel or a person appears under no WORK
    /// axis at all, because an audience is not a work: the two are different id
    /// spaces, and `run_state::submit` now writes neither work field for any of
    /// them. Widening the work-axis list cannot reach such an act.
    ///
    /// The record does now carry `audience`, and `acts_for_audience` enumerates
    /// by it — so the act is findable, just not by this book, which asks a
    /// question about WORK. Counting it here is still what stops the book
    /// reporting a complete enumeration of an incomplete one, and the count is
    /// now also the pointer to where those acts can be found.
    #[test]
    fn an_act_no_work_names_is_counted_rather_than_silently_missed() {
        let bench = bench();
        delivered_act(&bench, "a", None, Some("eng-1"), "payload-a@1");
        // Neither field set: exactly what an `AudienceKind::Account` run
        // submits.
        delivered_act(&bench, "b", None, None, "payload-b@1");

        let book = book_over(
            &bench,
            vec![declaration(
                "outreach",
                "v1",
                &["payload-a@1", "payload-b@1"],
            )],
        );
        let scopes = book.enumerate().expect("enumerate");
        assert_eq!(scopes[0].bindings.len(), 1);
        assert_eq!(
            scopes[0].acts_without_work, 1,
            "the account-bound act is invisible to every work axis and must be counted"
        );
    }

    /// A cohort component that is blank or carries the separator is refused.
    ///
    /// The version is the cohort key: a blank one makes the sample able only to
    /// agree with itself. U+001F is what joins an observation id's components,
    /// so a value carrying it could fold two different outcomes into one record
    /// and the second would resume the first instead of entering the sample.
    #[test]
    fn a_blank_or_separator_carrying_cohort_component_is_refused() {
        assert!(DeclaredPayloadVariants::new(&[declaration("outreach", "  ", &["p@1"])]).is_err());
        assert!(DeclaredPayloadVariants::new(&[declaration("", "v1", &["p@1"])]).is_err());
        assert!(
            DeclaredPayloadVariants::new(&[declaration("out\u{1f}reach", "v1", &["p@1"])]).is_err(),
            "a variant ref carrying U+001F could fold two outcomes into one observation id"
        );
        assert!(
            DeclaredPayloadVariants::new(&[declaration("outreach", "v\u{1f}1", &["p@1"])]).is_err()
        );
        assert!(
            DeclaredPayloadVariants::new(&[declaration("outreach", "v1", &["  "])]).is_err(),
            "a blank payload declares a cohort no act can join"
        );
        assert!(DeclaredPayloadVariants::new(&[declaration("outreach", "v1", &[])]).is_err());
    }

    /// An identical replay resumes; a changed payload is an error.
    #[test]
    fn one_payload_cannot_be_declared_into_two_cohorts() {
        let identical = DeclaredPayloadVariants::new(&[
            declaration("outreach", "v1", &["p@1"]),
            declaration("outreach", "v1", &["p@1"]),
        ])
        .expect("an identical repeat is one declaration");
        assert_eq!(identical.declared_payloads(), 1);

        let conflicting = DeclaredPayloadVariants::new(&[
            declaration("outreach", "v1", &["p@1"]),
            declaration("outreach", "v2", &["p@1"]),
        ]);
        let error = conflicting.expect_err("one act's outcome cannot be in two cohorts");
        assert!(
            error.to_string().contains("two"),
            "the error must name the confusion: {error}"
        );
    }

    /// A book naming no scope is refused at construction.
    ///
    /// It would otherwise start a loop that reports `degraded` on every tick —
    /// true, but only visible to somebody already watching the health snapshot,
    /// and by then the failure has stopped looking like the configuration error
    /// it is.
    #[test]
    fn a_book_with_no_scopes_is_refused_before_it_can_tick() {
        let bench = bench();
        assert!(WorkspaceMaturityBook::new(bench.layout.clone(), Vec::new()).is_err());
    }

    /// A settled act reads as settled however the window is changed afterwards.
    ///
    /// The non-negotiable rule, asserted through the real path rather than
    /// through `mature_silences` alone: sweep, then widen the window from two
    /// days to two hundred and sweep again. The second sweep must record
    /// nothing and must report the act as already settled — **not** as still
    /// open with a later maturity instant. That exact bug existed here:
    /// ripeness was derived before the store was consulted, so widening a
    /// window made an already-concluded act report as still waiting, and a
    /// caller acting on the report would chase a counterparty whose silence is
    /// a recorded fact.
    #[test]
    fn a_settled_act_stays_settled_when_the_window_is_widened() {
        let bench = bench();
        let act_ref = delivered_act(&bench, "a", None, Some("eng-1"), "payload-a@1");
        let book = book_over(
            &bench,
            vec![declaration("outreach", "v1", &["payload-a@1"])],
        );
        let scopes = book.enumerate().expect("enumerate");
        let outcomes = OutcomeStore::new(bench.layout.clone());
        let outcome_scope = OutcomeScope::new("owner", "work");

        let narrow = MaturityPolicy::new(chrono::Duration::days(2)).expect("policy");
        let first = run_maturity_sweep(
            &bench.store,
            &bench.scope,
            &outcomes,
            &outcome_scope,
            &scopes[0].bindings,
            &narrow,
            at(20, 0),
        )
        .expect("first sweep");
        assert_eq!(first.matured, vec![act_ref.clone()]);
        assert_eq!(first.already_settled_count(), 0);

        let widened = MaturityPolicy::new(chrono::Duration::days(200)).expect("policy");
        let second = run_maturity_sweep(
            &bench.store,
            &bench.scope,
            &outcomes,
            &outcome_scope,
            &scopes[0].bindings,
            &widened,
            at(20, 0),
        )
        .expect("second sweep");
        assert_eq!(
            second.matured,
            Vec::<String>::new(),
            "a second sweep must record nothing"
        );
        assert_eq!(
            second.already_settled_count(),
            1,
            "a decision already made must not move because the window changed"
        );
        assert_eq!(
            second.still_open_count(),
            0,
            "a settled act reported as still open is the exact bug this pins"
        );

        let observations = outcomes
            .observations_for_act(&outcome_scope, &act_ref)
            .expect("read back");
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].label, OutcomeLabel::Silent);
        assert_eq!(observations[0].variant_version, "v1");
        assert_eq!(observations[0].engagement_id.as_deref(), Some("eng-1"));
    }

    /// The sweep ships off, and an off switch does not read as a fault.
    #[tokio::test]
    async fn the_sweep_is_off_until_an_operator_turns_it_on() {
        let bench = bench();
        let config = OutcomeMaturityConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.default_window_days, 14);
        assert!(
            config.scopes.is_empty(),
            "a default that named a scope would sweep somebody's acts unasked"
        );

        let worker =
            spawn_configured_maturity_sweep(bench.layout.clone(), config, CancellationToken::new());
        let snapshot = worker.health().snapshot().await;
        assert_eq!(snapshot.state, "disabled");
        assert_eq!(snapshot.last_error, None);
    }

    /// An enabled sweep that names no scope is degraded, never started.
    ///
    /// The vacuous pass in its most dangerous form: the process boots, the
    /// worker exists, every count is zero, and nothing has ever been swept.
    #[tokio::test]
    async fn an_enabled_sweep_with_no_scopes_declines_visibly() {
        let bench = bench();
        let worker = spawn_configured_maturity_sweep(
            bench.layout.clone(),
            OutcomeMaturityConfig {
                enabled: true,
                ..OutcomeMaturityConfig::default()
            },
            CancellationToken::new(),
        );
        let snapshot = worker.health().snapshot().await;
        assert_eq!(snapshot.state, "degraded");
        assert!(
            snapshot
                .last_error
                .as_deref()
                .unwrap_or_default()
                .contains("book"),
            "the reason must name what could not be built: {:?}",
            snapshot.last_error
        );
    }

    /// A configured sweep produces a book over the scope it names.
    ///
    /// Pins the whole configuration path end to end — a YAML block reaching a
    /// binding — because every layer of it is correct in isolation and the
    /// programme's signature failure is the join nobody made.
    #[test]
    fn configuration_reaches_a_binding() {
        let bench = bench();
        let act_ref = delivered_act(&bench, "a", Some("prog-1"), None, "payload-a@1");
        let config = OutcomeMaturityConfig {
            enabled: true,
            scopes: vec![OutcomeMaturityScopeConfig {
                principal: "owner".to_string(),
                workspace: "work".to_string(),
                variants: vec![declaration("outreach", "v4", &["payload-a@1"])],
            }],
            ..OutcomeMaturityConfig::default()
        };
        let book = WorkspaceMaturityBook::from_config(bench.layout.clone(), &config).expect("book");
        let scopes = book.enumerate().expect("enumerate");
        assert_eq!(scopes.len(), 1);
        assert_eq!(scopes[0].bindings.len(), 1);
        assert_eq!(scopes[0].bindings[0].act_ref, act_ref);
        assert_eq!(scopes[0].bindings[0].variant_ref, "outreach");
        assert_eq!(scopes[0].bindings[0].variant_version, "v4");
    }

    /// The axis enumerator refuses the two axes that would answer wrongly.
    ///
    /// `recipient` holds assertion-use ids as well as act refs, so serving it
    /// as acts would have the book load ids that resolve to no act and count
    /// every one of them as unbound — a scope reported as undescribable when it
    /// is merely mixed. A traversing axis is worse: the enumerator LISTS a
    /// directory where `index_entries` reads one hashed file, so `..` would
    /// report another directory's contents as this scope's outward acts.
    #[test]
    fn the_axis_enumerator_refuses_a_mixed_axis_and_a_traversing_one() {
        let bench = bench();
        delivered_act(&bench, "a", None, Some("eng-1"), "payload-a@1");

        assert!(
            bench
                .store
                .act_refs_under_axis(&bench.scope, "recipient")
                .is_err(),
            "the recipient axis is mixed and must not be served as acts"
        );
        for axis in ["", "..", "../index", "engagement/../..", "a\\b"] {
            assert!(
                bench.store.act_refs_under_axis(&bench.scope, axis).is_err(),
                "`{axis}` must not address a directory outside the scope's index"
            );
        }
        // And the axes it does serve still answer.
        assert_eq!(
            bench
                .store
                .act_refs_under_axis(&bench.scope, WorkContextKind::ENGAGEMENT_TOKEN)
                .expect("engagement axis")
                .len(),
            1
        );
    }

    /// Every kind of work has an axis, checked against the enum itself.
    ///
    /// The `match` is exhaustive on purpose: a third kind of work added to
    /// [`WorkContextKind`] stops this compiling, which is the point. A kind
    /// missing from `KIND_TOKENS` would be silently unsweepable — its acts
    /// would be indexed under an axis the book never looks at, and the book
    /// would report a complete enumeration of an incomplete one.
    #[test]
    fn every_kind_of_work_carries_a_token_the_book_enumerates() {
        for kind in [
            WorkContextKind::Program("id".to_string()),
            WorkContextKind::Engagement("id".to_string()),
        ] {
            let token = match &kind {
                WorkContextKind::Program(_) => WorkContextKind::PROGRAM_TOKEN,
                WorkContextKind::Engagement(_) => WorkContextKind::ENGAGEMENT_TOKEN,
            };
            assert_eq!(kind.kind_token(), token);
            assert!(
                WorkContextKind::KIND_TOKENS.contains(&token),
                "`{token}` is not in the list the book enumerates, so its acts are unreachable"
            );
            assert!(WorkContextKind::from_token(token, "id").is_ok());
        }
        assert_eq!(WorkContextKind::KIND_TOKENS.len(), 2);
    }
}
