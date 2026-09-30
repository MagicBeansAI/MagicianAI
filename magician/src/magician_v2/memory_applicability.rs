//! Slice 3: choosing preferences by **applicability**, not similarity.
//!
//! The problem, from `docs/archive/plans/2026-08-13-cross-session-taste-design.md`:
//! cross-session taste is compressed to a handful of entries chosen by cosine
//! similarity to the goal, and *similarity is not applicability*. A
//! delivery-style preference can completely govern a research task while
//! sitting nowhere near it in embedding space; a preference that shares every
//! keyword with a task can fail to govern it at all. More embedding tuning
//! never reaches that, because it is a category error rather than a tuning gap.
//!
//! # What this is, and what it deliberately is not
//!
//! One bounded judge call decides which candidate preferences govern the task.
//! Nothing else.
//!
//! The design also specified a scope-tag pre-filter ahead of the judge, and it
//! was built and then removed on 2026-08-13. The reason is worth keeping so it
//! is not re-attempted from the design doc alone: **there is no write path by
//! which a tag could be persisted where selection would read it.** A
//! preference candidate's `metadata_json` is either a synthesized literal
//! (`{"candidate_kind": "tier"}`) or the preference's own stored value wrapped
//! as `{"semantic_memory_type": …, "value": <scalar>}`. Storing tags would
//! mean changing the shape of every stored preference — which also changes how
//! each renders as prompt text — to feed a pre-filter in front of the
//! component that actually fixes the category error. The judge does the work;
//! the tags were an optimisation in front of it, and they cost more than they
//! saved.
//!
//! # Failing safely is the whole design
//!
//! This sits in front of prompt assembly. **Every failure path returns the
//! incoming order unchanged**: unbound operation, no router, model error,
//! timeout, unparseable reply, a verdict set that does not match what was
//! asked. There is no failure here worth failing a prompt over, and the
//! incoming order is the ranking that shipped for years before this existed.

use std::{collections::BTreeSet, future::Future, pin::Pin, sync::Arc};

use serde::{Deserialize, Serialize};
use tracing::instrument;

use crate::magician_v2::analytics::runtime_activity_layer::KIND_BACKGROUND;

/// Operation the applicability judge binds to in `operation_mapping`.
///
/// **Unbound means the judge is off** and the existing ranking serves. The
/// router's `default_profile` is never consulted: silently putting a per-run
/// LLM call on the prompt hot path is not something a default may do.
pub const APPLICABILITY_JUDGE_OPERATION: &str = "memory_applicability_judge";

/// Wall-clock budget for one judge call.
///
/// Enforced locally rather than trusted to the provider — a provider that
/// hangs would otherwise hang every prompt. Exceeding it abandons the call and
/// serves the incoming order.
pub const JUDGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// How many candidates the judge is asked about.
///
/// The lane budget is far smaller than this, but narrowing happens before the
/// budget applies, so the cap bounds prompt size and latency rather than the
/// final selection.
pub const JUDGE_CANDIDATE_LIMIT: usize = 12;

/// One preference under consideration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub item_key: String,
    pub text: String,
}

/// A candidate offered to the judge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Narrowed {
    pub candidate: Candidate,
}

/// Take the first `limit` candidates, in the order given.
///
/// **This does not sort.** The caller passes candidates in the lane's own
/// ranking, which orders on `query_intent_priority`, then score, then
/// recency, then tier name — and that order is both the baseline this feature
/// is measured against and the fallback every failure path returns to.
///
/// An earlier version re-sorted on score alone. That silently discarded
/// `query_intent_priority`, the lane's *primary* key, so the "fallback equals
/// the status quo" guarantee was false before the judge was even consulted:
/// merely enabling the feature reordered preferences. The test that was meant
/// to pin this fed score-differentiated candidates, where score order and
/// input order coincide, so it passed either way.
///
/// The incoming order is already deterministic — the lane's sort has four
/// keys ending in `tier_name` — so no tiebreak is needed here and the verdict
/// cache key stays stable across identical runs.
pub fn narrow(mut candidates: Vec<Candidate>, limit: usize) -> Vec<Narrowed> {
    candidates.truncate(limit);
    candidates
        .into_iter()
        .map(|candidate| Narrowed { candidate })
        .collect()
}

/// A judge's answer for one candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub item_key: String,
    /// Does this preference actually govern the task at hand?
    pub governs: bool,
    /// One line the owner could read to understand the choice. A selection
    /// nobody can explain is one nobody can correct.
    #[serde(default)]
    pub rationale: String,
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub origin: Option<decision_engine_contract::classification::ClassificationOrigin>,
}

/// Fold judge verdicts into the incoming order.
///
/// Candidates the judge did not answer for keep their position rather than
/// being dropped. A partial answer is common — it ran out of budget, or only
/// mentioned what it was sure about — and treating silence as rejection would
/// let a truncated response empty the lane.
///
/// Rejected candidates are demoted, never removed: the budget truncates the
/// tail anyway, so a wrong verdict costs a position instead of an omission.
pub fn rank_with_verdicts(narrowed: Vec<Narrowed>, verdicts: &[Verdict]) -> Vec<Narrowed> {
    if verdicts.is_empty() {
        return narrowed;
    }
    let mut governs: Vec<Narrowed> = Vec::new();
    let mut unjudged: Vec<Narrowed> = Vec::new();
    let mut rejected: Vec<Narrowed> = Vec::new();

    for item in narrowed {
        match verdicts
            .iter()
            .find(|v| v.item_key == item.candidate.item_key)
        {
            Some(verdict) if verdict.governs => governs.push(item),
            Some(_) => rejected.push(item),
            None => unjudged.push(item),
        }
    }
    governs.extend(unjudged);
    governs.extend(rejected);
    governs
}

/// Cache key for a judge result.
///
/// Covers the **scope**, the goal, and each candidate's key *and text*. Every
/// one of those is load-bearing:
///
/// - **Scope.** The cache is process-global while `item_key` is not
///   scope-unique — a candidate's dedupe key is
///   `{User|Agent|AgentGoal}:{tier}:{item}`, which carries no principal or
///   workspace. Without the scope in the key, two owners whose
///   `preferences:tone` say opposite things share one entry, and whichever
///   asked first decides for both.
/// - **Text, not just key.** A preference edited in place keeps its key. Key
///   alone would keep applying a judgement made about what it used to say.
/// - **Order.** A differently-ordered candidate set is a different question,
///   and the answer is an ordering.
pub fn verdict_cache_key(
    principal: &str,
    workspace: &str,
    goal: &str,
    narrowed: &[Narrowed],
) -> String {
    let mut hasher = blake3::Hasher::new();
    // Length-prefixed rather than delimiter-joined: a separator can appear
    // inside a principal, a goal or a preference, and concatenation would let
    // two different inputs hash the same.
    for part in [principal, workspace, goal] {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    for item in narrowed {
        for part in [
            item.candidate.item_key.as_str(),
            item.candidate.text.as_str(),
        ] {
            hasher.update(&(part.len() as u64).to_le_bytes());
            hasher.update(part.as_bytes());
        }
    }
    hasher.finalize().to_hex()[..24].to_string()
}

/// Parse a judge reply into verdicts.
///
/// Malformed output yields *no* verdicts, so the caller serves the incoming
/// order rather than acting on a half-understood answer. Verdicts for keys
/// that were not offered are dropped — a model that invents an item key must
/// not be able to reorder something the caller never showed it.
pub fn parse_verdicts(raw: &str, offered: &[Narrowed]) -> Vec<Verdict> {
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default)]
        verdicts: Vec<Verdict>,
    }
    let Ok(envelope) = serde_json::from_str::<Envelope>(raw.trim()) else {
        return Vec::new();
    };
    let known: BTreeSet<&str> = offered
        .iter()
        .map(|n| n.candidate.item_key.as_str())
        .collect();
    envelope
        .verdicts
        .into_iter()
        .filter(|verdict| known.contains(verdict.item_key.as_str()))
        .collect()
}

/// Render the candidate list the judge is asked about.
/// How much of one candidate's text the judge reads. Applicability is
/// decided from what a memory is about, not from its whole body; a candidate
/// carrying a dumped document once made a 466k-token judge prompt.
pub const JUDGED_CANDIDATE_MAX_BYTES: usize = 1_500;

fn judged_candidate_head(text: &str) -> std::borrow::Cow<'_, str> {
    let text = text.trim();
    if text.len() <= JUDGED_CANDIDATE_MAX_BYTES {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut cut = JUDGED_CANDIDATE_MAX_BYTES;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    std::borrow::Cow::Owned(format!("{}…", &text[..cut]))
}

pub fn render_candidates_for_judge(narrowed: &[Narrowed]) -> String {
    narrowed
        .iter()
        .map(|item| {
            format!(
                "- [{}] {}",
                item.candidate.item_key,
                judged_candidate_head(&item.candidate.text)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Bounded process-global cache of judge verdicts.
///
/// **This is what makes the judge affordable at all.** The design budgets "one
/// call per run/resume/steer", but the render path it hangs off runs on every
/// prompt assembly — many times per run. Without a cache the judge would add
/// its latency and its cost to each of those, which is a different feature
/// from the one that was designed.
static VERDICT_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, Vec<Verdict>)>>,
> = std::sync::OnceLock::new();

/// How long a verdict stays usable.
///
/// Short by design: the cache exists to collapse the many prompt assemblies
/// within one run, not to remember judgements across a working session. A long
/// TTL would keep applying a verdict to preferences the owner has since edited
/// — the key covers *which* preferences were judged, not their contents.
const VERDICT_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// Entries kept before the cache is cleared.
///
/// A hard bound rather than an LRU: this is a latency optimisation on a hot
/// path, and the eviction policy mattering would mean the cache is doing more
/// work than the calls it saves. Clearing wholesale costs at most one extra
/// judge call per distinct goal.
const VERDICT_CACHE_MAX: usize = 256;

fn verdict_cache(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, Vec<Verdict>)>>
{
    VERDICT_CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn cached_verdicts(key: &str) -> Option<Vec<Verdict>> {
    let mut guard = match verdict_cache().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    match guard.get(key) {
        Some((stored_at, verdicts)) if stored_at.elapsed() < VERDICT_TTL => Some(verdicts.clone()),
        Some(_) => {
            guard.remove(key);
            None
        },
        None => None,
    }
}

fn store_verdicts(key: String, verdicts: Vec<Verdict>) {
    let mut guard = match verdict_cache().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.len() >= VERDICT_CACHE_MAX {
        guard.clear();
    }
    guard.insert(key, (std::time::Instant::now(), verdicts));
}

/// Ask the judge which preferences govern this task, or return them unchanged.
///
/// `goal` is the user's task text and `narrowed` holds preference bodies, so
/// `skip_all` is the difference between a diagnostic row and leaking the
/// prompt. Only the candidate count is named.
#[instrument(
    name = "memory_applicability_judge",
    skip_all,
    fields(
        activity_kind = KIND_BACKGROUND,
        principal = %principal,
        workspace = %workspace,
        candidates = narrowed.len(),
    )
)]
pub async fn judge_or_fall_back(
    narrowed: Vec<Narrowed>,
    goal: &str,
    router: Option<&crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter>,
    principal: &str,
    workspace: &str,
) -> Vec<Narrowed> {
    judge_or_fall_back_with_source_check(narrowed, goal, router, principal, workspace, None).await
}

pub type ApplicabilitySourceCheck =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

pub async fn judge_or_fall_back_with_source_check(
    narrowed: Vec<Narrowed>,
    goal: &str,
    router: Option<&crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter>,
    principal: &str,
    workspace: &str,
    source_check_factory: Option<&(dyn Fn() -> ApplicabilitySourceCheck + Send + Sync)>,
) -> Vec<Narrowed> {
    use crate::magician_v2::{
        decision_host::{
            self,
            classification::{self, PolicyLookup},
        },
        decisions::{observation, reference, runner},
    };
    use decision_engine_contract::{batch::DecisionItem, request::DecisionState};
    if narrowed.len() < 2 {
        return narrowed;
    }
    let Some(router) = router else {
        return narrowed;
    };
    let prompt_version =
        crate::magician_v2::prompts::constants::versions::MEMORY_APPLICABILITY_JUDGE;
    let Some(reference_version) =
        reference::version(router, APPLICABILITY_JUDGE_OPERATION, prompt_version)
    else {
        return narrowed;
    };
    let started = std::time::Instant::now();
    let generation = decision_host::generation();
    // Policy precedes cache: an engine entry can never retain authority across Off/reload.
    if goal.len() > 32768 {
        return narrowed;
    }
    let policy = classification::ready_policy("memory_applicability", principal, workspace).await;
    let policy_key = match &policy {
        PolicyLookup::Participating(p) => format!(
            "{}:{}:{}:{}",
            p.engine_instance,
            p.revision,
            p.policy.gate,
            p.policy.classification.behavior_fingerprint
        ),
        _ => "incumbent".into(),
    };
    let case_id = verdict_cache_key(principal, workspace, goal, &narrowed);
    let cache_key =
        format!("{case_id}:{reference_version}:{generation}:{policy_key}:applicability-v1");
    if let Some(verdicts) = cached_verdicts(&cache_key) {
        crate::magician_v2::decisions::telemetry::invocation(
            principal,
            workspace,
            "memory_applicability",
            &case_id,
            narrowed.len(),
            "application_cache_hit",
            started.elapsed(),
        );
        return rank_with_verdicts(narrowed, &verdicts);
    }
    let offered: Vec<_> = narrowed
        .iter()
        .take(JUDGE_CANDIDATE_LIMIT)
        .cloned()
        .collect();
    let mut input = runner::Input {
        operation: "memory_applicability".into(),
        projection_version: "memory-applicability-v1".into(),
        reference_version: reference_version.clone(),
        case_id,
        // Oversized context falls back rather than silently changing the question.
        context: (goal.len() <= 32768)
            .then(|| DecisionState::from_json(serde_json::json!({"goal":goal}))),
        items: offered
            .iter()
            .enumerate()
            .map(|(i, n)| DecisionItem {
                item_id: i.to_string(),
                state: DecisionState::from_json(
                    serde_json::json!({"memory":judged_candidate_head(&n.candidate.text)}),
                ),
                choice_candidates: Default::default(),
            })
            .collect(),
        required_questions: vec!["applicable".into()],
        scope: router.classification_trace_context(magicllm::LlmScope::new(principal, workspace)),
        agent: None,
        requires_completion: false,
        replay: None,
    };
    if let (PolicyLookup::Participating(participation), Some(source_check_factory)) =
        (&policy, source_check_factory)
    {
        if observation::selected(&input, participation) {
            type Snapshot = (Vec<Narrowed>, String);
            if let Some((snapshot, bytes)) =
                observation::snapshot_bounded::<_, Snapshot>(&(&offered, goal))
            {
                let source_check = source_check_factory();
                let snapshot = Arc::new(snapshot);
                let check_router = router.clone();
                let check_revision = reference_version.clone();
                let check_scope = magicllm::LlmScope::new(principal, workspace);
                let run_router = router.clone();
                let run_scope = check_scope.clone();
                input.replay = Some(Ok(runner::ReferenceReplay {
                    bytes,
                    cost_reservation_microusd: reference::observation_cost_upper_microusd(
                        router,
                        APPLICABILITY_JUDGE_OPERATION,
                        bytes,
                    ),
                    current: Box::new(move || {
                        let router = check_router.clone();
                        let revision = check_revision.clone();
                        let scope = check_scope.clone();
                        let source_check = source_check.clone();
                        Box::pin(async move {
                            router.observation_dispatch_available()
                                && router.authoritative_trace_scope().as_ref() == Some(&scope)
                                && reference::version(
                                    &router,
                                    APPLICABILITY_JUDGE_OPERATION,
                                    crate::magician_v2::prompts::constants::versions::MEMORY_APPLICABILITY_JUDGE,
                                ).as_ref() == Some(&revision)
                                && source_check().await
                        })
                    }),
                    access_current: None,
                    run: Box::new(move || {
                        Box::pin(async move {
                            let started = std::time::Instant::now();
                            let Some(prompt) = judge_prompt(&snapshot.0, &snapshot.1).await else {
                                return runner::ReplayResult {
                                    status: "prompt_unavailable",
                                    attempted: Some(false),
                                    reference: None,
                                };
                            };
                            let response = reference::pinned_json_observation(
                                &run_router,
                                APPLICABILITY_JUDGE_OPERATION,
                                &run_scope.principal,
                                &run_scope.workspace,
                                &prompt,
                                &snapshot.1,
                            )
                            .await;
                            let Ok(response) = response else {
                                return runner::ReplayResult {
                                    status: "failed",
                                    attempted: None,
                                    reference: None,
                                };
                            };
                            let parsed = parse_checked_verdicts(&response.content, &snapshot.0);
                            let labels = parsed
                                .as_ref()
                                .map(|verdicts| {
                                    verdicts
                                        .iter()
                                        .filter_map(|verdict| {
                                            let index =
                                                snapshot.0.iter().position(|candidate| {
                                                    candidate.candidate.item_key == verdict.item_key
                                                })?;
                                            Some((
                                                index.to_string(),
                                                std::collections::BTreeMap::from([(
                                                    "applicable".into(),
                                                    serde_json::json!(verdict.governs),
                                                )]),
                                            ))
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            runner::ReplayResult {
                                status: if parsed.is_some() { "completed" } else { "failed" },
                                attempted: Some(true),
                                reference: Some(crate::magician_v2::decisions::telemetry::Reference::from_response(
                                    labels,
                                    &response,
                                    started.elapsed().as_millis() as u64,
                                )),
                            }
                        })
                    }),
                }));
            } else {
                input.replay = Some(Err("snapshot_oversize"));
            }
        }
    }
    let outcome = runner::run(
        input,
        policy,
        JUDGE_TIMEOUT,
        std::time::Duration::from_secs(2),
        |ids, budget| {
            let selected: Vec<_> = ids
                .iter()
                .filter_map(|id| id.parse::<usize>().ok().and_then(|i| offered.get(i)))
                .cloned()
                .collect();
            incumbent_verdicts(selected, goal, router, principal, workspace, budget)
        },
        |incumbent: &ApplicabilityIncumbent| {
            let labels = incumbent
                .verdicts
                .iter()
                .filter_map(|v| {
                    let id = offered
                        .iter()
                        .position(|n| n.candidate.item_key == v.item_key)?
                        .to_string();
                    Some((
                        id,
                        std::collections::BTreeMap::from([(
                            "applicable".into(),
                            serde_json::json!(v.governs),
                        )]),
                    ))
                })
                .collect();
            crate::magician_v2::decisions::telemetry::Reference::from_response(
                labels,
                &incumbent.response,
                incumbent.latency_ms,
            )
        },
    )
    .await;
    let engine_answers =
        if reference::version(router, APPLICABILITY_JUDGE_OPERATION, prompt_version).as_deref()
            == Some(reference_version.as_str())
        {
            outcome.current_answers()
        } else {
            Default::default()
        };
    let incumbent_valid = outcome.incumbent.is_some();
    let mut verdicts = outcome.incumbent.map(|i| i.verdicts).unwrap_or_default();
    for (id, answers) in &engine_answers {
        let Some(candidate) = id.parse::<usize>().ok().and_then(|i| offered.get(i)) else {
            continue;
        };
        let Some(value) = answers.get("applicable").and_then(|a| a.noul_value()) else {
            continue;
        };
        verdicts.push(Verdict {
            item_key: candidate.candidate.item_key.clone(),
            governs: value >= 0.5,
            rationale: "Qualified applicability classification; model provenance is attached."
                .into(),
            origin: outcome.origins.get(id).cloned(),
        });
    }
    // A transient failure is never a successful empty five-minute entry.
    if (incumbent_valid || engine_answers.len() == offered.len())
        && generation == decision_host::generation()
        && reference::version(router, APPLICABILITY_JUDGE_OPERATION, prompt_version).as_deref()
            == Some(reference_version.as_str())
    {
        store_verdicts(cache_key, verdicts.clone());
    }
    rank_with_verdicts(narrowed, &verdicts)
}

struct ApplicabilityIncumbent {
    verdicts: Vec<Verdict>,
    response: crate::magician_v2::query_analysis::operation_llm_router::SimplifiedLLMResponse,
    latency_ms: u64,
}

async fn incumbent_verdicts(
    offered: Vec<Narrowed>,
    goal: &str,
    router: &crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter,
    principal: &str,
    workspace: &str,
    budget: std::time::Duration,
) -> Option<ApplicabilityIncumbent> {
    let started = std::time::Instant::now();
    let prompt = judge_prompt(&offered, goal).await?;
    let response = tokio::time::timeout(
        budget,
        crate::magician_v2::decisions::reference::pinned_json(
            router,
            APPLICABILITY_JUDGE_OPERATION,
            principal,
            workspace,
            &prompt,
            goal,
        ),
    )
    .await
    .ok()?
    .ok()?;
    let verdicts = parse_checked_verdicts(&response.content, &offered)?;
    Some(ApplicabilityIncumbent {
        verdicts,
        response,
        latency_ms: started.elapsed().as_millis() as u64,
    })
}

async fn judge_prompt(offered: &[Narrowed], goal: &str) -> Option<String> {
    let mut variables = std::collections::HashMap::new();
    variables.insert("goal".into(), goal.to_string());
    variables.insert("candidates".into(), render_candidates_for_judge(&offered));
    crate::magician_v2::prompts::rendered_prompt(
        crate::magician_v2::prompts::constants::names::MEMORY_APPLICABILITY_JUDGE,
        crate::magician_v2::prompts::constants::versions::MEMORY_APPLICABILITY_JUDGE,
        variables,
    )
    .await
    .ok()
}

fn parse_checked_verdicts(raw: &str, offered: &[Narrowed]) -> Option<Vec<Verdict>> {
    #[derive(Deserialize)]
    struct Envelope {
        verdicts: Vec<Verdict>,
    }
    let response: Envelope = serde_json::from_str(raw.trim()).ok()?;
    let mut ids = BTreeSet::new();
    if response.verdicts.iter().any(|v| {
        !offered.iter().any(|n| n.candidate.item_key == v.item_key) || !ids.insert(&v.item_key)
    }) {
        return None;
    }
    Some(response.verdicts)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn candidate(key: &str) -> Candidate {
        Candidate {
            item_key: key.to_string(),
            text: format!("preference {key}"),
        }
    }

    fn verdict(key: &str, governs: bool) -> Verdict {
        Verdict {
            item_key: key.to_string(),
            governs,
            rationale: String::new(),
            origin: None,
        }
    }

    #[test]
    fn narrowing_preserves_the_incoming_order_exactly() {
        // The keys are deliberately NOT in any order this module could
        // reconstruct — not alphabetical, not by any field it can see. The
        // earlier version of this test used score-ordered input, so a sort on
        // score looked identical to preserving input order and the test passed
        // against a function that discarded the lane's primary sort key.
        let given = vec![candidate("zebra"), candidate("alpha"), candidate("middle")];
        let out = narrow(given.clone(), 12);
        let keys: Vec<&str> = out.iter().map(|n| n.candidate.item_key.as_str()).collect();
        assert_eq!(keys, vec!["zebra", "alpha", "middle"]);
        assert_eq!(out.len(), given.len());
    }

    #[test]
    fn the_cache_key_is_stable_across_identical_runs() {
        // The incoming order is already deterministic (the lane sorts on four
        // keys ending in tier_name), so no tiebreak is needed here — but the
        // cache key must still be reproducible or every assembly misses.
        let build = || narrow(vec![candidate("b"), candidate("a"), candidate("c")], 12);
        let key = |n: &[Narrowed]| verdict_cache_key("p", "w", "goal", n);
        assert_eq!(key(&build()), key(&build()));
        // Order is part of the identity: a differently-ordered set is a
        // different question, and the answer is an ordering.
        let reversed = narrow(vec![candidate("c"), candidate("a"), candidate("b")], 12);
        assert_ne!(key(&build()), key(&reversed));
    }

    #[test]
    fn the_candidate_limit_is_enforced() {
        let many: Vec<Candidate> = (0..40).map(|i| candidate(&format!("k{i:02}"))).collect();
        assert_eq!(
            narrow(many, JUDGE_CANDIDATE_LIMIT).len(),
            JUDGE_CANDIDATE_LIMIT
        );
    }

    #[test]
    fn no_verdicts_leaves_the_order_untouched() {
        // The timeout path, and the most important property in the module.
        let one = narrow(vec![candidate("a"), candidate("b")], 12);
        assert_eq!(rank_with_verdicts(one.clone(), &[]), one);
    }

    #[test]
    fn unjudged_candidates_keep_their_place_rather_than_being_dropped() {
        // A partial judge answer is common; treating silence as rejection
        // would let a truncated response empty the lane.
        let narrowed = narrow(
            vec![
                candidate("judged-yes"),
                candidate("silent"),
                candidate("judged-no"),
            ],
            12,
        );
        let out = rank_with_verdicts(
            narrowed,
            &[verdict("judged-yes", true), verdict("judged-no", false)],
        );
        let keys: Vec<&str> = out.iter().map(|n| n.candidate.item_key.as_str()).collect();
        assert_eq!(keys, vec!["judged-yes", "silent", "judged-no"]);
        assert_eq!(
            out.len(),
            3,
            "a rejected candidate is demoted, never removed"
        );
    }

    #[test]
    fn a_low_scoring_but_applicable_preference_beats_a_high_scoring_one() {
        // The whole point of the slice, expressed as an ordering: relevance
        // said "high" first; the judge says only "low" governs the task.
        let narrowed = narrow(vec![candidate("high"), candidate("low")], 12);
        assert_eq!(
            narrowed[0].candidate.item_key, "high",
            "input order preserved"
        );
        let out = rank_with_verdicts(narrowed, &[verdict("low", true), verdict("high", false)]);
        assert_eq!(out[0].candidate.item_key, "low");
    }

    #[test]
    fn a_malformed_judge_reply_yields_no_verdicts() {
        let offered = narrow(vec![candidate("a"), candidate("b")], 12);
        for raw in ["", "not json", "{}", "null", "{\"verdicts\": \"nope\"}"] {
            assert!(parse_verdicts(raw, &offered).is_empty(), "raw {raw:?}");
        }
    }

    #[test]
    fn a_verdict_for_an_unoffered_key_is_dropped() {
        // A model inventing an item key must not reorder something the caller
        // never showed it.
        let offered = narrow(vec![candidate("real"), candidate("other")], 12);
        let raw = serde_json::json!({
            "verdicts": [
                {"item_key": "real", "governs": true, "rationale": "yes"},
                {"item_key": "hallucinated", "governs": true, "rationale": "no such thing"}
            ]
        })
        .to_string();
        let parsed = parse_verdicts(&raw, &offered);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].item_key, "real");
    }

    /// The judge decides whether a memory governs a goal; it does not need
    /// the memory's whole body to do that. One candidate carrying a dumped
    /// document made a run-start judge prompt of 466k tokens (10 s, paid on
    /// every run), so each candidate is judged on a bounded head of its text.
    #[test]
    fn the_judge_reads_a_bounded_head_of_each_candidate() {
        let mut huge = candidate("huge");
        huge.text = "x".repeat(50_000);
        let mut multibyte = candidate("multibyte");
        multibyte.text = "é".repeat(JUDGED_CANDIDATE_MAX_BYTES);
        let offered = narrow(vec![huge, multibyte, candidate("small")], 12);
        let rendered = render_candidates_for_judge(&offered);
        assert!(
            rendered.len() < 2 * JUDGED_CANDIDATE_MAX_BYTES + 200,
            "rendered {} bytes",
            rendered.len()
        );
        assert!(
            rendered.contains("[huge]") && rendered.contains("…"),
            "{rendered}"
        );
        assert!(rendered.contains("[multibyte]"));
        assert!(rendered.contains("- [small] preference small"));
    }

    #[test]
    fn the_judge_sees_keys_it_can_answer_with() {
        let offered = narrow(vec![candidate("k1"), candidate("k2")], 12);
        let rendered = render_candidates_for_judge(&offered);
        assert!(rendered.contains("[k1]") && rendered.contains("[k2]"));
        let raw = serde_json::json!({
            "verdicts": [{"item_key": "k1", "governs": true, "rationale": "r"}]
        })
        .to_string();
        assert_eq!(parse_verdicts(&raw, &offered).len(), 1);
    }

    fn unique_key() -> String {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEED: AtomicUsize = AtomicUsize::new(0);
        format!("cache-{}", SEED.fetch_add(1, Ordering::Relaxed))
    }

    #[test]
    fn the_verdict_cache_round_trips_by_key() {
        let key = unique_key();
        assert!(cached_verdicts(&key).is_none(), "cold key must miss");
        let verdicts = vec![verdict("a", true)];
        store_verdicts(key.clone(), verdicts.clone());
        assert_eq!(cached_verdicts(&key), Some(verdicts));
        assert!(cached_verdicts(&format!("{key}-other")).is_none());
    }

    #[test]
    fn the_cache_is_bounded() {
        for i in 0..(VERDICT_CACHE_MAX + 10) {
            store_verdicts(format!("bound-{i}"), Vec::new());
        }
        let guard = verdict_cache().lock().unwrap();
        assert!(
            guard.len() <= VERDICT_CACHE_MAX,
            "cache grew to {}",
            guard.len()
        );
    }

    #[test]
    fn the_cache_key_separates_scopes_goals_sets_and_edits() {
        let a = narrow(vec![candidate("a")], 12);
        let b = narrow(vec![candidate("a"), candidate("b")], 12);
        // Candidate set, and goal.
        assert_ne!(
            verdict_cache_key("p", "w", "goal", &a),
            verdict_cache_key("p", "w", "goal", &b)
        );
        assert_ne!(
            verdict_cache_key("p", "w", "goal-1", &a),
            verdict_cache_key("p", "w", "goal-2", &a)
        );
        // Scope. item_key is NOT scope-unique, so without this two owners
        // whose `preferences:tone` say opposite things share one entry.
        assert_ne!(
            verdict_cache_key("owner-a", "w", "goal", &a),
            verdict_cache_key("owner-b", "w", "goal", &a)
        );
        assert_ne!(
            verdict_cache_key("p", "space-1", "goal", &a),
            verdict_cache_key("p", "space-2", "goal", &a)
        );
        // An in-place edit keeps the key but changes the text, and the old
        // judgement must not survive it.
        let edited = narrow(
            vec![Candidate {
                item_key: "a".into(),
                text: "rewritten".into(),
            }],
            12,
        );
        assert_ne!(
            verdict_cache_key("p", "w", "goal", &a),
            verdict_cache_key("p", "w", "goal", &edited)
        );
    }

    #[test]
    fn cache_key_parts_cannot_be_confused_by_concatenation() {
        // Length-prefixed hashing: a separator can occur inside a principal,
        // a goal or a preference, so naive joining would let two different
        // inputs collide.
        let one = narrow(vec![candidate("a")], 12);
        assert_ne!(
            verdict_cache_key("ab", "c", "goal", &one),
            verdict_cache_key("a", "bc", "goal", &one)
        );
    }

    #[tokio::test]
    async fn the_judge_falls_back_without_a_router() {
        let narrowed = narrow(vec![candidate("a"), candidate("b")], 12);
        let out = judge_or_fall_back(narrowed.clone(), "some goal", None, "p", "w").await;
        assert_eq!(out, narrowed, "the existing order must serve unchanged");
    }

    #[tokio::test]
    async fn the_judge_is_a_no_op_below_two_candidates() {
        for n in 0..2 {
            let input: Vec<Narrowed> = (0..n)
                .map(|i| Narrowed {
                    candidate: candidate(&format!("k{i}")),
                })
                .collect();
            let out = judge_or_fall_back(input.clone(), "goal", None, "p", "w").await;
            assert_eq!(out, input);
        }
    }
}
