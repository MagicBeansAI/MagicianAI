//! Relevance labels: which kinds of act each advisory intention can bear on.
//!
//! Most of what Magician has inferred about an owner is about how work gets
//! done ("prefer minimal code changes", "verify against system state"), not
//! about who gets an email or what it says. Judging all of it before every
//! outward act made each check a few hundred model calls (2026-10-25). Each
//! intention is therefore labelled, once per revision and act kind, by the
//! Decision Engine's `memory_applicability` pack (operation
//! [`RELEVANCE_ENGINE_OPERATION`]): does this preference apply to an act of
//! this kind? A check then judges only the advisory intentions labelled
//! relevant to its act, plus every one not labelled yet.
//!
//! The labels only ever narrow the advisory lane. The blocking lane (the
//! owner's stated or confirmed preferences) is never filtered, an unlabelled
//! or relabelled intention is judged until its new label lands, and an engine
//! that is off, unbound or failing labels nothing, so the check judges the
//! whole lane as before.
//!
//! Labels live in `memory/users/preference_alignment_relevance.json`
//! (`alignment_relevance.v1`), written under an exclusive flock, keyed by
//! intention id and checked against its revision. Labelling runs in the
//! background, one job per owner scope and act kind at a time.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use decision_engine_contract::batch::DecisionItem;
use decision_engine_contract::request::DecisionState;
use serde::{Deserialize, Serialize};

use super::contract::ActionFacts;
use super::intention::Intention;
use crate::magician_v2::agents::storage::{AgentStorage, AgentStorageError};
use crate::magician_v2::agents::AgentMemoryResolver;
use crate::magician_v2::decision_host::classification::{ready_policy, PolicyLookup};
use crate::magician_v2::decisions::runner;

/// The Decision Engine operation that labels relevance.
pub const RELEVANCE_ENGINE_OPERATION: &str = "preference_relevance";
pub const RELEVANCE_FILE: &str = "preference_alignment_relevance.json";
pub const RELEVANCE_SCHEMA_VERSION: &str = "alignment_relevance.v1";
/// Bumped when the question or the act descriptions change, so old labels
/// are not read as answers to the new question.
const LABELER_VERSION: &str = "memory_applicability@1.0.0/relevance-v1";
/// At or above this applicability an intention is relevant. Low on purpose:
/// only a clear "does not apply" removes an intention from the check.
const RELEVANT_AT: f64 = 0.3;
/// Intentions asked about per engine call, and per background job.
const LABEL_CHUNK: usize = 24;
const LABEL_JOB_MAX: usize = 512;
const LABEL_BUDGET: Duration = Duration::from_secs(90);
const STATEMENT_MAX_BYTES: usize = 1_500;

/// The kind of act an intention is labelled against: the act's outward class
/// when it has one, else its consequence class.
pub fn act_kind(facts: &ActionFacts) -> String {
    facts
        .outward_class
        .clone()
        .unwrap_or_else(|| facts.consequence_class.clone())
}

/// What the labeler is told the act is. Unknown kinds get a generic line.
fn act_description(kind: &str) -> String {
    let what = match kind {
        "mail" => "sending an email to another person: who receives it, what it says, its tone, timing and attachments",
        "message" => "sending a chat message (WhatsApp, Telegram, iMessage) to another person: who receives it, what it says, its tone and timing",
        "calendar_invite" => "sending a calendar invitation: who is invited, when, for how long, and what it says",
        "form_submission" => "submitting a form or application on someone else's website: what is entered and sent",
        "bounded_communication" => "sending a message to someone already in the conversation",
        "confidential_disclosure" => "sharing confidential or non-public information with someone",
        "submission_or_publication" => "publishing or submitting something publicly",
        "commitment_or_transaction" => "making a purchase, booking, payment or binding commitment",
        _ => "taking an action that leaves the owner's computer",
    };
    format!(
        "The assistant is about to act for the owner: {what}. Does this owner preference bear on how \
         that act should be done or whether it should be done? Preferences about how the assistant \
         does its own work (coding, research method, tooling, internal process) do not apply."
    )
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct RelevanceEntry {
    revision: String,
    /// Act kind → relevant.
    kinds: BTreeMap<String, bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct RelevanceFile {
    schema_version: String,
    labeler: String,
    #[serde(default)]
    entries: BTreeMap<String, RelevanceEntry>,
}

impl Default for RelevanceFile {
    fn default() -> Self {
        Self {
            schema_version: RELEVANCE_SCHEMA_VERSION.to_string(),
            labeler: LABELER_VERSION.to_string(),
            entries: BTreeMap::new(),
        }
    }
}

impl RelevanceFile {
    /// The label for `intention` against `kind`, if one is current.
    fn label(&self, intention: &Intention, kind: &str) -> Option<bool> {
        self.entries
            .get(&intention.id)
            .filter(|entry| entry.revision == intention.revision)
            .and_then(|entry| entry.kinds.get(kind).copied())
    }
}

/// The advisory intentions this check should judge, the ones still
/// unlabelled for this act kind, and the ones left out. An intention is left
/// out only when its current label says it does not apply.
pub fn narrow(
    intentions: Vec<Intention>,
    labels: &BTreeMap<String, (String, Option<bool>)>,
) -> (Vec<Intention>, Vec<Intention>, Vec<Intention>) {
    let mut kept = Vec::with_capacity(intentions.len());
    let mut unlabelled = Vec::new();
    let mut dropped = Vec::new();
    for intention in intentions {
        match labels
            .get(&intention.id)
            .filter(|(revision, _)| revision == &intention.revision)
            .and_then(|(_, relevant)| *relevant)
        {
            Some(false) => dropped.push(intention),
            Some(true) => kept.push(intention),
            None => {
                unlabelled.push(intention.clone());
                kept.push(intention);
            },
        }
    }
    (kept, unlabelled, dropped)
}

/// Read the labels for `intentions` against `kind`: id → (revision the
/// label was for, label). A missing, corrupt or stale-labeler file reads as
/// no labels.
pub async fn read_labels(
    resolver: &AgentMemoryResolver,
    principal: &str,
    workspace: &str,
    intentions: &[Intention],
    kind: &str,
) -> BTreeMap<String, (String, Option<bool>)> {
    let Ok(storage) = storage_for(resolver, principal, workspace) else {
        return BTreeMap::new();
    };
    let file = read_file(&storage, &relevance_path(&storage)).await;
    intentions
        .iter()
        .map(|intention| {
            (
                intention.id.clone(),
                (intention.revision.clone(), file.label(intention, kind)),
            )
        })
        .collect()
}

type JobKey = (String, String, String);

fn in_flight() -> &'static Mutex<HashSet<JobKey>> {
    static JOBS: OnceLock<Mutex<HashSet<JobKey>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Holds a job's slot; dropping it frees the slot even if the job panics or
/// is cancelled, so a failed job cannot stop labelling for the process.
struct JobSlot(JobKey);

impl JobSlot {
    fn claim(key: JobKey) -> Option<Self> {
        let mut jobs = in_flight()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        jobs.insert(key.clone()).then(|| Self(key))
    }
}

impl Drop for JobSlot {
    fn drop(&mut self) {
        in_flight()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&self.0);
    }
}

/// Label `unlabelled` against `kind` in the background. At most one job per
/// owner scope and act kind runs at a time; a later check picks up whatever
/// this one did not reach. `live`, when the inventory was complete, is every
/// intention id in it: labels for any other id (a deleted preference) are
/// dropped when this job writes.
pub fn spawn_labelling(
    resolver: AgentMemoryResolver,
    principal: String,
    workspace: String,
    kind: String,
    mut unlabelled: Vec<Intention>,
    live: Option<BTreeSet<String>>,
) {
    if unlabelled.is_empty() {
        return;
    }
    let Some(slot) = JobSlot::claim((principal.clone(), workspace.clone(), kind.clone())) else {
        return;
    };
    unlabelled.truncate(LABEL_JOB_MAX);
    tokio::spawn(Box::pin(async move {
        let _slot = slot;
        let labelled = label(
            &resolver,
            &principal,
            &workspace,
            &kind,
            &unlabelled,
            live.as_ref(),
        )
        .await;
        tracing::info!(
            act_kind = %kind,
            asked = unlabelled.len(),
            labelled = labelled.len(),
            relevant = labelled.iter().filter(|(_, _, relevant)| *relevant).count(),
            "[PREFERENCE-ALIGNMENT] relevance labels"
        );
    }));
}

/// Ask the engine, chunk by chunk, recording each chunk's labels as it
/// lands. Returns (id, revision, relevant) for every intention it answered;
/// the rest stay unlabelled.
async fn label(
    resolver: &AgentMemoryResolver,
    principal: &str,
    workspace: &str,
    kind: &str,
    intentions: &[Intention],
    live: Option<&BTreeSet<String>>,
) -> Vec<(String, String, bool)> {
    let mut labelled = Vec::new();
    for chunk in intentions.chunks(LABEL_CHUNK) {
        let lookup = ready_policy(RELEVANCE_ENGINE_OPERATION, principal, workspace).await;
        match &lookup {
            PolicyLookup::Participating(participation) if participation.policy.gate => {},
            // Off, unbound or unavailable: label nothing; checks judge all.
            _ => return labelled,
        }
        let context = serde_json::json!({ "task": act_description(kind) });
        let items: Vec<DecisionItem> = chunk
            .iter()
            .enumerate()
            .map(|(index, intention)| DecisionItem {
                item_id: index.to_string(),
                state: DecisionState::from_json(serde_json::json!({
                    "memory": clip(&intention.statement, STATEMENT_MAX_BYTES),
                    "conditions": intention.conditions.as_deref().map(|c| clip(c, 400)),
                    "kind": intention.kind,
                    "trust": intention.assertion_status,
                })),
                choice_candidates: BTreeMap::new(),
            })
            .collect();
        let case_id = blake3::hash(
            &serde_json::to_vec(&serde_json::json!([LABELER_VERSION, kind, &items]))
                .unwrap_or_default(),
        )
        .to_hex()
        .to_string();
        let mut trace = magicllm::LlmTraceContext::new(
            magicllm::LlmScope::new(principal, workspace),
            magicllm::LlmWorkloadClass::Ambient,
        );
        trace.call_role = magicllm::LlmCallRole::Judge;
        let request = runner::Input {
            operation: RELEVANCE_ENGINE_OPERATION.to_string(),
            projection_version: "preference-relevance-v1".to_string(),
            reference_version: LABELER_VERSION.to_string(),
            case_id,
            context: Some(DecisionState::from_json(context)),
            items,
            required_questions: vec!["applicable".to_string()],
            scope: trace,
            agent: None,
            requires_completion: false,
            replay: None,
        };
        let outcome = runner::run::<(), _, _, _>(
            request,
            lookup,
            LABEL_BUDGET,
            Duration::ZERO,
            |_, _| async { None },
            |_: &()| BTreeMap::new().into(),
        )
        .await;
        let mut answered = Vec::new();
        for (item_id, answers) in outcome.current_answers() {
            let Some(intention) = item_id.parse::<usize>().ok().and_then(|i| chunk.get(i)) else {
                continue;
            };
            let Some(value) = answers.get("applicable").and_then(|a| a.noul_value()) else {
                continue;
            };
            answered.push((
                intention.id.clone(),
                intention.revision.clone(),
                value >= RELEVANT_AT,
            ));
        }
        if answered.is_empty() {
            continue;
        }
        if let Err(error) =
            record_labels(resolver, principal, workspace, kind, &answered, live).await
        {
            tracing::warn!(error = %error, "[PREFERENCE-ALIGNMENT] could not record relevance labels");
        }
        labelled.extend(answered);
    }
    labelled
}

async fn record_labels(
    resolver: &AgentMemoryResolver,
    principal: &str,
    workspace: &str,
    kind: &str,
    labelled: &[(String, String, bool)],
    live: Option<&BTreeSet<String>>,
) -> Result<(), String> {
    let storage = storage_for(resolver, principal, workspace)?;
    let path = relevance_path(&storage);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| error.to_string())?;
    }
    let _flock = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| error.to_string())?;
    let mut file = read_file(&storage, &path).await;
    // Only this write grows the file, so pruning here bounds it by the live
    // inventory.
    if let Some(live) = live {
        file.entries.retain(|id, _| live.contains(id));
    }
    for (id, revision, relevant) in labelled {
        let entry = file.entries.entry(id.clone()).or_default();
        if &entry.revision != revision {
            // A new revision starts over: labels for the old text are void.
            *entry = RelevanceEntry {
                revision: revision.clone(),
                kinds: BTreeMap::new(),
            };
        }
        entry.kinds.insert(kind.to_string(), *relevant);
    }
    storage
        .write_json_atomic(&path, &file)
        .await
        .map_err(|error| error.to_string())
}

fn storage_for(
    resolver: &AgentMemoryResolver,
    principal: &str,
    workspace: &str,
) -> Result<AgentStorage, String> {
    resolver
        .resolve_for_scope(principal, workspace)
        .map(|memory| memory.storage().clone())
        .map_err(|error| error.to_string())
}

pub fn relevance_path(storage: &AgentStorage) -> PathBuf {
    storage.user_root().join(RELEVANCE_FILE)
}

/// The labels file, or an empty one when it is missing, unreadable, from
/// another schema or written by another labeler version.
async fn read_file(storage: &AgentStorage, path: &PathBuf) -> RelevanceFile {
    match storage.read_bytes(path).await {
        Ok(bytes) => match serde_json::from_slice::<RelevanceFile>(&bytes) {
            Ok(file)
                if file.schema_version == RELEVANCE_SCHEMA_VERSION
                    && file.labeler == LABELER_VERSION =>
            {
                file
            },
            _ => RelevanceFile::default(),
        },
        Err(AgentStorageError::Io(io)) if io.kind() == std::io::ErrorKind::NotFound => {
            RelevanceFile::default()
        },
        Err(_) => RelevanceFile::default(),
    }
}

fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::preference_alignment::intention::{
        AssertionStatus, IntentionKind, IntentionLifecycle, IntentionSource,
        INTENTION_SCHEMA_VERSION,
    };

    fn intention(id: &str, revision: &str) -> Intention {
        Intention {
            schema_version: INTENTION_SCHEMA_VERSION.to_string(),
            id: id.to_string(),
            revision: revision.to_string(),
            source: IntentionSource::TasteProfile,
            kind: IntentionKind::Taste,
            assertion_status: AssertionStatus::Inferred,
            statement: format!("statement {id}"),
            conditions: None,
            scope: None,
            lifecycle: IntentionLifecycle::Active,
            valid_until: None,
            evidence_refs: Vec::new(),
            intervention_policy: None,
        }
    }

    #[test]
    fn only_a_current_does_not_apply_label_drops_an_intention() {
        let labels = BTreeMap::from([
            ("relevant".to_string(), ("r1".to_string(), Some(true))),
            ("irrelevant".to_string(), ("r1".to_string(), Some(false))),
            // Labelled for an older revision: judged and relabelled.
            ("edited".to_string(), ("r0".to_string(), Some(false))),
            ("unasked".to_string(), ("r1".to_string(), None)),
        ]);
        let (kept, unlabelled, dropped) = narrow(
            vec![
                intention("relevant", "r1"),
                intention("irrelevant", "r1"),
                intention("edited", "r1"),
                intention("unasked", "r1"),
                intention("unknown", "r1"),
            ],
            &labels,
        );
        let ids = |list: &[Intention]| list.iter().map(|i| i.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&kept), ["relevant", "edited", "unasked", "unknown"]);
        assert_eq!(ids(&unlabelled), ["edited", "unasked", "unknown"]);
        assert_eq!(ids(&dropped), ["irrelevant"]);
    }

    #[tokio::test]
    async fn labels_round_trip_per_revision_and_act_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(tmp.path());
        let process = intention("process", "r1");
        let tone = intention("tone", "r1");
        record_labels(
            &resolver,
            "owner",
            "default",
            "mail",
            &[
                ("process".to_string(), "r1".to_string(), false),
                ("tone".to_string(), "r1".to_string(), true),
            ],
            None,
        )
        .await
        .unwrap();
        let both = [process.clone(), tone.clone()];
        let mail = read_labels(&resolver, "owner", "default", &both, "mail").await;
        assert_eq!(mail["process"].1, Some(false));
        assert_eq!(mail["tone"].1, Some(true));
        // Another act kind is not labelled by this one.
        let purchase = read_labels(
            &resolver,
            "owner",
            "default",
            &both,
            "commitment_or_transaction",
        )
        .await;
        assert_eq!(purchase["process"].1, None);
        // A new revision voids the old label.
        let edited = [intention("process", "r2")];
        let after_edit = read_labels(&resolver, "owner", "default", &edited, "mail").await;
        assert_eq!(after_edit["process"].1, None);
    }

    #[tokio::test]
    async fn a_deleted_preference_loses_its_labels_on_the_next_write() {
        let tmp = tempfile::tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(tmp.path());
        let label = |id: &str| (id.to_string(), "r1".to_string(), true);
        record_labels(
            &resolver,
            "owner",
            "default",
            "mail",
            &[label("kept"), label("deleted")],
            None,
        )
        .await
        .unwrap();
        let live = BTreeSet::from(["kept".to_string(), "new".to_string()]);
        record_labels(
            &resolver,
            "owner",
            "default",
            "mail",
            &[label("new")],
            Some(&live),
        )
        .await
        .unwrap();
        let storage = storage_for(&resolver, "owner", "default").unwrap();
        let file = read_file(&storage, &relevance_path(&storage)).await;
        assert_eq!(
            file.entries.keys().cloned().collect::<Vec<_>>(),
            ["kept", "new"],
            "the deleted preference's label is gone"
        );
    }
}
