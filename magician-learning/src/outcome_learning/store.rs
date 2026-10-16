//! Durable outcome observations — plan phase 1.
//!
//! *"Purely additive… Do this early even if the rest waits — the data cannot be
//! reconstructed afterwards."*
//!
//! So this records and reads, and does nothing else. No proposals, no scoring,
//! no aggregation beyond a cohort filter. §2 is explicit that the guardrail
//! comes before the loop, and the guardrails live here: silence is refused
//! before its window closes, and the cohort key is not optional.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

use super::types::{OutcomeLabel, OutcomeObservation, RecordOutcome};

/// Field separator for the observation id, the cohort key and the cohort index
/// line. Every caller string that feeds one of those — the scope's principal and
/// workspace, the act ref, the variant ref, the variant version — is refused if
/// it holds one. A value hiding a separator moves the boundary between two
/// components: variant `pitch<U+001F>a` at version `v1` joins to exactly what
/// variant `pitch` at version `a<U+001F>v1` joins to, so two different cohorts
/// would derive one observation id and one index file. Refusing it also means an
/// index line always splits where it was joined.
const FIELD_SEP: char = '\u{1f}';

/// Scope for a store call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutcomeScope {
    pub principal: String,
    pub workspace: String,
}

impl OutcomeScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
        }
    }
}

/// One `(variant, version)` cohort the store actually holds.
///
/// Counts and instants only. There is deliberately no engagement rate, no
/// score and no "better than" here: this says what exists, and every judgement
/// about it belongs to the phase that compares two of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordedCohort {
    pub variant_ref: String,
    pub variant_version: String,
    /// The earliest observation in it — when this version started producing
    /// outcomes. **Not** when the version was authored: nothing records that,
    /// and this is the closest honest answer the store can give.
    pub first_observed_at: DateTime<Utc>,
    /// How many observations resolved, so a reader can tell a cohort holding
    /// one row from one holding thirty without loading either.
    pub observations: usize,
}

/// Append-only store of what happened after we acted.
#[derive(Debug, Clone)]
pub struct OutcomeStore {
    workspace_layout: ArtifactV2Workspace,
}

impl OutcomeStore {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    fn root(&self, scope: &OutcomeScope) -> PathBuf {
        self.workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("outcome_learning")
    }

    fn act_path(&self, scope: &OutcomeScope, act_ref: &str) -> PathBuf {
        self.root(scope)
            .join("acts")
            .join(format!("{}.jsonl", stable_id(act_ref)))
    }

    /// Index by cohort key, written when the observation is.
    ///
    /// The cohort comparison is the entire point of the plan, so the pointers
    /// are written rather than derived by a later scan — a scan would be correct
    /// and would also be the thing nobody runs.
    fn cohort_path(&self, scope: &OutcomeScope, variant_ref: &str, version: &str) -> PathBuf {
        self.root(scope).join("cohorts").join(format!(
            "{}.jsonl",
            stable_id(&format!("{variant_ref}{FIELD_SEP}{version}"))
        ))
    }

    /// Record one outcome.
    ///
    /// # Two guardrails, both refusals
    ///
    /// **Silence before its window closes is refused.** Silence is the absence
    /// of a signal, so before maturity it is indistinguishable from "not yet".
    /// Recording it early would fill the sample with counterparties who simply
    /// had not replied by Tuesday, and every later comparison would inherit
    /// that.
    ///
    /// **An empty cohort key is refused.** It is what separates learning from
    /// self-confirmation: without knowing which proposal version was live, a
    /// before/after comparison cannot be made and the loop can only agree with
    /// itself.
    ///
    /// Separately: **U+001F is refused** in the scope, the act ref, the variant
    /// ref and the variant version. It is what joins the observation id's
    /// components and the cohort index line's two halves, so a value carrying
    /// one could make two different outcomes derive a single id — the second
    /// resuming the first and never entering the sample.
    ///
    /// Idempotent on `(act, variant, version, label)`: re-observing the same
    /// outcome — a poller that runs twice, a replayed webhook — resumes rather
    /// than inflating the sample. A count that grows on retry is the quietest
    /// possible way to make a small sample look significant.
    pub fn record(
        &self,
        scope: &OutcomeScope,
        request: &RecordOutcome,
        now: DateTime<Utc>,
    ) -> Result<OutcomeObservation> {
        validate_scope(scope)?;
        if request.variant_version.trim().is_empty() {
            anyhow::bail!(
                "an outcome with no variant_version cannot be compared against anything, so it \
                 can only ever confirm what is already believed"
            );
        }
        if request.act_ref.trim().is_empty() {
            anyhow::bail!("an outcome must name the act it is an outcome of");
        }
        // The three caller strings that feed the observation id, the cohort key
        // and the index line. Unchecked, a crafted one shifts a component
        // boundary and two different outcomes derive one id — the second
        // resuming the first instead of being recorded.
        reject_separator(&request.act_ref, "an act ref")?;
        reject_separator(&request.variant_ref, "a variant ref")?;
        reject_separator(&request.variant_version, "a variant version")?;

        if request.label.requires_maturity() {
            match request.matured_at {
                None => anyhow::bail!(
                    "`{}` needs a maturity time: before its window closes, silence is \
                     indistinguishable from `not yet`",
                    request.label.as_str()
                ),
                Some(matured) if now < matured => anyhow::bail!(
                    "`{}` does not mature until {matured}; recording it at {now} would label a \
                     counterparty who simply has not replied yet",
                    request.label.as_str()
                ),
                Some(_) => {},
            }
        }

        let observation_id = derive_observation_id(scope, request);
        let existing = self.load_observation(scope, &request.act_ref, &observation_id)?;

        if let Some(existing) = existing.as_ref() {
            // Identical re-observation: a poller running twice, a replayed
            // webhook. Nothing to write.
            if existing.delivery_state == request.delivery_state
                && existing.matured_at == request.matured_at
                && existing.confounders == request.confounders
            {
                return Ok(existing.clone());
            }
            // Something we know about the act has changed — almost always the
            // delivery state, which arrives after the outcome it describes.
            // Appended as a correction and folded last-wins, so the sample size
            // does not grow but the record stops being wrong.
        }

        let observation = OutcomeObservation {
            observation_id: observation_id.clone(),
            engagement_id: request.engagement_id.clone(),
            program_id: request.program_id.clone(),
            act_ref: request.act_ref.clone(),
            variant_ref: request.variant_ref.clone(),
            variant_version: request.variant_version.clone(),
            label: request.label,
            delivery_state: request.delivery_state,
            // First sight, not the moment of correction. A delivery update
            // arriving on Friday must not make the outcome look like it was
            // observed on Friday.
            observed_at: existing
                .as_ref()
                .map_or(now, |existing| existing.observed_at),
            matured_at: request.matured_at,
            confounders: request.confounders.clone(),
        };

        // A correction is already indexed; re-appending would add a duplicate
        // pointer the cohort read then has to skip.
        //
        // Index before the row, so "the row exists" implies "the index exists".
        // The other order loses the cohort pointer on a crash, and the retry
        // early-returns on the row it can see — the same ordering the outward
        // assertions store settled on, for the same reason.
        if existing.is_none() {
            self.append_cohort_index(scope, &observation)?;
        }
        self.append_json(&self.act_path(scope, &request.act_ref), &observation)?;
        Ok(observation)
    }

    /// Every observation recorded against one act, in the order first observed.
    ///
    /// # Later lines supersede earlier ones with the same id
    ///
    /// The file is a log, so a delivery correction is appended rather than
    /// edited in. Keeping the FIRST line would make those corrections
    /// unreachable: an act recorded `delivered` that a provider later reports as
    /// bounced would stay `delivered` for ever and be counted as a counterparty
    /// ignoring us when nothing arrived — the confusion §3 calls the most
    /// misleading available here.
    ///
    /// Position is held at first sight while the content comes from the last, so
    /// a correction updates a row without reordering the act's history.
    pub fn observations_for_act(
        &self,
        scope: &OutcomeScope,
        act_ref: &str,
    ) -> Result<Vec<OutcomeObservation>> {
        validate_scope(scope)?;
        reject_separator(act_ref, "an act ref")?;
        let path = self.act_path(scope, act_ref);
        let Some(raw) = self.read_if_present(&path)? else {
            return Ok(Vec::new());
        };
        let mut position: HashMap<String, usize> = HashMap::new();
        let mut out: Vec<OutcomeObservation> = Vec::new();
        // Tolerant of a torn tail only — see `magician_v2::jsonl`.
        for observation in
            magician::magician_v2::jsonl::parse_log_lines::<OutcomeObservation>(&raw, &path)?
        {
            match position.get(&observation.observation_id) {
                Some(&at) => out[at] = observation,
                None => {
                    position.insert(observation.observation_id.clone(), out.len());
                    out.push(observation);
                },
            }
        }
        Ok(out)
    }

    /// Every observation in one cohort — a `(variant, version)` pair.
    ///
    /// This is the read the whole plan is built around: comparing the same
    /// variant across versions is what shows whether a change helped, and it is
    /// why accepted proposals are **kept** in the sample rather than excluded.
    pub fn cohort(
        &self,
        scope: &OutcomeScope,
        variant_ref: &str,
        variant_version: &str,
    ) -> Result<Vec<OutcomeObservation>> {
        validate_scope(scope)?;
        // The pair is the cohort key. A separator in either half would let
        // `(pitch<U+001F>a, v1)` and `(pitch, a<U+001F>v1)` name one file, and a
        // read of one cohort would answer with another's sample.
        reject_separator(variant_ref, "a variant ref")?;
        reject_separator(variant_version, "a variant version")?;
        let path = self.cohort_path(scope, variant_ref, variant_version);
        let Some(raw) = self.read_if_present(&path)? else {
            return Ok(Vec::new());
        };

        // Read each act's file ONCE.
        //
        // The obvious loop — resolve every index entry independently — re-reads
        // and re-parses a whole act file per observation in it, so an act with
        // five outcomes costs five full parses and a cohort spanning one act
        // degrades quadratically. Cohort reads are the plan's central query, so
        // this is the one place here worth not being naive about.
        let mut wanted: Vec<(String, String)> = Vec::new();
        let mut seen = BTreeSet::new();
        for line in raw.lines().map(str::trim).filter(|line| !line.is_empty()) {
            // A line that will not split is corruption, and the fold refuses
            // rather than reading past it — the same rule
            // `magician_v2::jsonl::parse_log_lines` applies to a terminated line
            // it cannot parse. `append_cohort_index` is the only writer of this
            // file and always joins an act ref and an observation id with
            // FIELD_SEP, and neither half can carry one — the act ref is refused
            // it at record time, and the observation id is derived rather than
            // supplied — so a line without the separator was not written here.
            // Reading past it would drop an observation out of the cohort and
            // shrink the sample without saying so, and a comparison run on a
            // silently short sample is exactly what the guardrails above exist
            // to prevent.
            let Some((act_ref, observation_id)) = line.split_once(FIELD_SEP) else {
                anyhow::bail!(
                    "cohort index {} holds a line with no U+001F separator: every line in it is \
                     an act ref and an observation id joined by one, so a line without it is \
                     corruption — skipping it would drop an observation out of the cohort and \
                     report a short sample as a whole one",
                    path.display()
                );
            };
            if seen.insert(observation_id.to_string()) {
                wanted.push((act_ref.to_string(), observation_id.to_string()));
            }
        }

        let mut by_act: HashMap<String, HashMap<String, OutcomeObservation>> = HashMap::new();
        for (act_ref, _) in &wanted {
            if by_act.contains_key(act_ref) {
                continue;
            }
            let loaded = self
                .observations_for_act(scope, act_ref)?
                .into_iter()
                .map(|observation| (observation.observation_id.clone(), observation))
                .collect();
            by_act.insert(act_ref.clone(), loaded);
        }

        // Index order is preserved: it is grant order, which is the order an
        // owner reading a cohort expects.
        Ok(wanted
            .iter()
            .filter_map(|(act_ref, observation_id)| {
                by_act
                    .get(act_ref)
                    .and_then(|found| found.get(observation_id))
                    .cloned()
            })
            .collect())
    }

    /// Every `(variant, version)` cohort this scope has actually recorded.
    ///
    /// # Why the store answers this and not a caller
    ///
    /// The cohort files are named by a hash of `variant<U+001F>version`, so the
    /// pair is not recoverable from the filesystem — only from the observations
    /// the index points at. A caller that reconstructed this would need this
    /// store's path layout, which is the second-authoritative-copy failure the
    /// whole subsystem is arranged against.
    ///
    /// Without it the only way to compare two versions is for somebody to type
    /// both of them somewhere, and a comparison nobody types is a comparison
    /// that never happens — which is how a loop that records evidence ends up
    /// never surfacing any.
    ///
    /// # The readings that matter
    ///
    /// - **`first_observed_at` is the earliest observation in the cohort**, not
    ///   the file's order. It is what lets a caller say which version was live
    ///   earlier without the store inventing a version lineage nobody recorded.
    /// - **A pointer with no row is skipped, exactly as [`cohort`](Self::cohort)
    ///   skips it.** The index is written before the row, so a crash between the
    ///   two leaves a pointer that resolves to nothing and a retry repairs it.
    ///   Two readers of the same file must not disagree about that.
    /// - **A cohort whose every pointer dangles is not returned at all.** It has
    ///   no observations, so reporting it would offer a comparison with nothing
    ///   in it — and an empty cohort satisfies a floor of zero vacuously, which
    ///   is the exact shape of the bug the floors exist to stop.
    /// - **Two different cohort keys in one file is corruption, not a merge.**
    ///   `append_cohort_index` derives the path from the pair, so a file holding
    ///   two pairs means a hash collision or a hand-edited log; folding them
    ///   would report one cohort's sample under another's name.
    ///
    /// Sorted by variant, then by first observation, then by version, so two
    /// reads of an unchanged scope agree and a caller pairing consecutive
    /// versions gets the same pairs every time.
    pub fn recorded_cohorts(&self, scope: &OutcomeScope) -> Result<Vec<RecordedCohort>> {
        validate_scope(scope)?;
        let dir = self.root(scope).join("cohorts");
        let paths = magician::magician_v2::jsonl::list_log_paths(&self.workspace_layout, &dir)?;

        // One parse per act across every cohort file, not per pointer: an act
        // with five outcomes in three cohorts would otherwise be parsed fifteen
        // times.
        let mut by_act: HashMap<String, HashMap<String, OutcomeObservation>> = HashMap::new();
        let mut out: Vec<RecordedCohort> = Vec::new();

        for path in paths {
            let Some(raw) = self.read_if_present(&path)? else {
                continue;
            };
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let mut cohort: Option<RecordedCohort> = None;

            for line in raw.lines().map(str::trim).filter(|line| !line.is_empty()) {
                // Same refusal as `cohort`: reading past a line that will not
                // split would drop an observation and report a short sample as
                // a whole one.
                let Some((act_ref, observation_id)) = line.split_once(FIELD_SEP) else {
                    anyhow::bail!(
                        "cohort index {} holds a line with no U+001F separator: every line in it \
                         is an act ref and an observation id joined by one, so a line without it \
                         is corruption — skipping it would drop an observation out of the cohort \
                         and report a short sample as a whole one",
                        path.display()
                    );
                };
                if !seen.insert(observation_id.to_string()) {
                    continue;
                }
                if !by_act.contains_key(act_ref) {
                    let loaded = self
                        .observations_for_act(scope, act_ref)?
                        .into_iter()
                        .map(|observation| (observation.observation_id.clone(), observation))
                        .collect();
                    by_act.insert(act_ref.to_string(), loaded);
                }
                let Some(observation) = by_act
                    .get(act_ref)
                    .and_then(|held| held.get(observation_id))
                else {
                    // Index before row. See the doc note.
                    continue;
                };

                match cohort.as_mut() {
                    None => {
                        cohort = Some(RecordedCohort {
                            variant_ref: observation.variant_ref.clone(),
                            variant_version: observation.variant_version.clone(),
                            first_observed_at: observation.observed_at,
                            observations: 1,
                        });
                    },
                    Some(held) => {
                        if held.variant_ref != observation.variant_ref
                            || held.variant_version != observation.variant_version
                        {
                            anyhow::bail!(
                                "cohort index {} holds observations from two different cohorts, \
                                 `{}`/`{}` and `{}`/`{}`: the file's own name is derived from the \
                                 pair, so folding them would report one cohort's sample under the \
                                 other's name",
                                path.display(),
                                held.variant_ref,
                                held.variant_version,
                                observation.variant_ref,
                                observation.variant_version,
                            );
                        }
                        held.observations += 1;
                        if observation.observed_at < held.first_observed_at {
                            held.first_observed_at = observation.observed_at;
                        }
                    },
                }
            }

            if let Some(cohort) = cohort {
                out.push(cohort);
            }
        }

        out.sort_by(|left, right| {
            left.variant_ref
                .cmp(&right.variant_ref)
                .then_with(|| left.first_observed_at.cmp(&right.first_observed_at))
                .then_with(|| left.variant_version.cmp(&right.variant_version))
        });
        Ok(out)
    }

    /// The cohort, filtered to what may actually be used as evidence.
    ///
    /// Separate from [`cohort`](Self::cohort) rather than folded into it: the
    /// full set is what an owner should see, and the usable subset is what a
    /// comparison may run on. Collapsing them would make "we observed thirty"
    /// and "thirty are comparable" the same number, which they are not.
    pub fn usable_cohort(
        &self,
        scope: &OutcomeScope,
        variant_ref: &str,
        variant_version: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<OutcomeObservation>> {
        Ok(self
            .cohort(scope, variant_ref, variant_version)?
            .into_iter()
            .filter(|observation| observation.is_usable_evidence(now))
            .collect())
    }

    fn load_observation(
        &self,
        scope: &OutcomeScope,
        act_ref: &str,
        observation_id: &str,
    ) -> Result<Option<OutcomeObservation>> {
        Ok(self
            .observations_for_act(scope, act_ref)?
            .into_iter()
            .find(|observation| observation.observation_id == observation_id))
    }

    fn append_cohort_index(
        &self,
        scope: &OutcomeScope,
        observation: &OutcomeObservation,
    ) -> Result<()> {
        let path = self.cohort_path(
            scope,
            &observation.variant_ref,
            &observation.variant_version,
        );
        let mut line = format!(
            "{}{FIELD_SEP}{}",
            observation.act_ref, observation.observation_id
        )
        .into_bytes();
        line.push(b'\n');
        magician::magician_v2::jsonl::append_log_line(&self.workspace_layout, &path, &line)
            .with_context(|| format!("appending cohort index {}", path.display()))?;
        Ok(())
    }

    fn append_json<T: Serialize>(&self, path: &PathBuf, value: &T) -> Result<()> {
        let mut line = serde_json::to_vec(value)?;
        line.push(b'\n');
        magician::magician_v2::jsonl::append_log_line(&self.workspace_layout, path, &line)
            .with_context(|| format!("appending {}", path.display()))?;
        Ok(())
    }

    fn read_if_present(&self, path: &PathBuf) -> Result<Option<String>> {
        // NotFound is the only error that reads as an empty store. Everything
        // else propagates: an unreadable log folded to "empty" fails open —
        // guards pass vacuously and removals report success while removing
        // nothing. Shared semantics live in `magician_v2::jsonl`.
        magician::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, path)
    }
}

fn stable_id(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..32].to_string()
}

/// A caller string that feeds an id derivation or the cohort index line.
///
/// Refused if it holds U+001F, because that is what joins the components: an act
/// ref carrying one could shift a boundary so that two different `(act, variant,
/// version, label)` tuples hash to one observation id, and the second outcome
/// would resume the first instead of being recorded — a sample that quietly
/// stops growing.
fn reject_separator(value: &str, what: &str) -> Result<()> {
    if value.contains(FIELD_SEP) {
        anyhow::bail!(
            "{what} must not contain U+001F: it is the separator that keeps an observation \
             id's components — and the cohort index line's two halves — from bleeding into \
             each other, and a value carrying it could fold two different outcomes into one \
             record"
        );
    }
    Ok(())
}

fn validate_scope(scope: &OutcomeScope) -> Result<()> {
    if scope.principal.contains(FIELD_SEP) || scope.workspace.contains(FIELD_SEP) {
        anyhow::bail!(
            "a scope's principal and workspace must not contain U+001F: it is the separator \
             that keeps an observation id's components from bleeding into each other, and a \
             scope component carrying it could fold two owners' outcomes into one sample"
        );
    }
    Ok(())
}

/// The id for one observation.
///
/// Derived from `(act, variant, version, label)` rather than allocated, so
/// re-observing the same outcome resumes instead of inflating the sample. The
/// label is in the key because one act legitimately produces several outcomes
/// over time — accepted, then delivered, then replied — and those are different
/// observations rather than a correction of each other.
fn derive_observation_id(scope: &OutcomeScope, request: &RecordOutcome) -> String {
    format!(
        "obs-{}",
        stable_id(&format!(
            "{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}",
            scope.principal,
            scope.workspace,
            request.act_ref,
            request.variant_ref,
            request.variant_version,
            request.label.as_str(),
        ))
    )
}

/// A label that supersedes silence.
///
/// Exposed so a caller that recorded `Silent` and later sees a reply knows the
/// two coexist rather than conflict: the silence was true of its window, and the
/// reply is true of a later one. Nothing is rewritten, because an outcome that
/// can be edited afterwards is not evidence.
pub fn supersedes_silence(label: OutcomeLabel) -> bool {
    label.is_engagement()
}
