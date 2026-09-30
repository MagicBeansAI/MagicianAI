//! Bounded, observation-only reference work. The production runner never awaits it.
use super::{
    runner::{Input, ReferenceReplay},
    telemetry::{Observation, Reference},
};
use crate::magician_v2::decision_host::classification::Participation;
use decision_engine_contract::classification::ClassificationObservationPolicy;
use serde::{de::DeserializeOwned, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};

const MAX_JOBS: usize = 16;
const MAX_SCOPE_JOBS: usize = 4;
const MAX_SNAPSHOT: usize = 256 * 1024;
const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_GLOBAL_HOURLY: usize = 60;
const MAX_SCOPE_HOURLY: usize = 10;
const TTL: Duration = Duration::from_secs(300);
const HOUR: Duration = Duration::from_secs(3600);
static WORKER: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(1)));
static JOBS: LazyLock<Mutex<Jobs>> = LazyLock::new(|| Mutex::new(Jobs::default()));

struct Limited(Vec<u8>);
impl std::io::Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_SNAPSHOT {
            return Err(std::io::Error::other("reference snapshot exceeds 256 KiB"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Serialize through a hard byte cap before owning a replay snapshot. Owners
/// call this only after deterministic selection, so unsampled inputs are not
/// cloned or retained for background inference.
pub(crate) fn snapshot_bounded<T: Serialize, U: DeserializeOwned>(
    source: &T,
) -> Option<(U, usize)> {
    let mut output = Limited(Vec::new());
    serde_json::to_writer(&mut output, source).ok()?;
    let bytes = output.0.len();
    Some((serde_json::from_slice(&output.0).ok()?, bytes))
}

type Scope = (String, String);
type CaseKey = (Scope, String, String, String, String);
#[derive(Default)]
struct Jobs {
    active: BTreeSet<CaseKey>,
    completed: VecDeque<(CaseKey, Instant)>,
    scopes: BTreeMap<Scope, usize>,
    bytes: usize,
    global_hour: VecDeque<Instant>,
    scope_hour: BTreeMap<Scope, VecDeque<Instant>>,
    global_spend: VecDeque<(Instant, u64)>,
}
struct Job {
    key: CaseKey,
    bytes: usize,
}
impl Job {
    fn admit(
        input: &Input,
        revision: &str,
        policy: &ClassificationObservationPolicy,
        bytes: usize,
        cost_microusd: Result<u64, &'static str>,
    ) -> Result<Self, &'static str> {
        let cost_microusd = cost_microusd?;
        if bytes > MAX_SNAPSHOT || bytes > policy.max_snapshot_bytes {
            return Err("snapshot_oversize");
        }
        let scope = (
            input.scope.scope.principal.clone(),
            input.scope.scope.workspace.clone(),
        );
        let key = (
            scope.clone(),
            input.operation.clone(),
            input.case_id.clone(),
            input.reference_version.clone(),
            revision.to_owned(),
        );
        let mut jobs = JOBS.lock().unwrap_or_else(|poison| poison.into_inner());
        let now = Instant::now();
        jobs.global_hour.retain(|at| now.duration_since(*at) < HOUR);
        jobs.global_spend
            .retain(|(at, _)| now.duration_since(*at) < HOUR);
        jobs.scope_hour
            .entry(scope.clone())
            .or_default()
            .retain(|at| now.duration_since(*at) < HOUR);
        jobs.completed
            .retain(|(_, at)| now.duration_since(*at) < TTL);
        if jobs
            .completed
            .iter()
            .any(|(completed, _)| completed == &key)
        {
            return Err("completed_duplicate");
        }
        if jobs.active.contains(&key) {
            return Err("duplicate");
        }
        if jobs.active.len() >= policy.max_pending.min(MAX_JOBS)
            || jobs.scopes.get(&scope).copied().unwrap_or(0)
                >= policy.max_per_scope.min(MAX_SCOPE_JOBS)
            || jobs.bytes.saturating_add(bytes) > policy.max_retained_bytes.min(MAX_BYTES)
        {
            return Err("queue_full");
        }
        if jobs.global_hour.len() >= policy.max_per_hour.min(MAX_GLOBAL_HOURLY)
            || jobs.scope_hour[&scope].len() >= policy.max_scope_per_hour.min(MAX_SCOPE_HOURLY)
        {
            return Err("hourly_limit");
        }
        let reserved: u64 = jobs.global_spend.iter().map(|(_, amount)| *amount).sum();
        if reserved.saturating_add(cost_microusd)
            > policy.max_reserved_microusd_per_hour.min(10_000_000)
        {
            return Err("spending_limit");
        }
        jobs.active.insert(key.clone());
        *jobs.scopes.entry(scope.clone()).or_default() += 1;
        jobs.bytes += bytes;
        jobs.global_hour.push_back(now);
        jobs.global_spend.push_back((now, cost_microusd));
        jobs.scope_hour.get_mut(&scope).unwrap().push_back(now);
        Ok(Self { key, bytes })
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        let mut jobs = JOBS.lock().unwrap_or_else(|poison| poison.into_inner());
        jobs.active.remove(&self.key);
        jobs.bytes = jobs.bytes.saturating_sub(self.bytes);
        // Every admitted case has a terminal outcome, including queue expiry,
        // source revocation and provider failure. Retrying an identical case
        // within the retention window would count the same selected input more
        // than once in the observation report.
        jobs.completed.push_back((self.key.clone(), Instant::now()));
        while jobs.completed.len() > 1024 {
            jobs.completed.pop_front();
        }
        if let Some(count) = jobs.scopes.get_mut(&self.key.0) {
            *count -= 1;
            if *count == 0 {
                jobs.scopes.remove(&self.key.0);
            }
        }
    }
}

fn sampled(input: &Input, behavior: &str, observation_revision: &str, rate: f64) -> bool {
    if rate <= 0.0 {
        return false;
    }
    if rate >= 1.0 {
        return true;
    }
    let identity = serde_json::to_vec(&(
        &input.scope.scope.principal,
        &input.scope.scope.workspace,
        &input.operation,
        &input.case_id,
        &input.reference_version,
        behavior,
        observation_revision,
        "gate_sample_v1",
    ))
    .unwrap_or_default();
    let hash = blake3::hash(&identity);
    let bucket = u64::from_le_bytes(hash.as_bytes()[..8].try_into().unwrap());
    (bucket as f64 / u64::MAX as f64) < rate
}

fn receipt_without_labels(reference: Option<Reference>) -> Option<Reference> {
    reference.map(|mut reference| {
        reference.labels.clear();
        reference
    })
}

pub(crate) fn selected(input: &Input, participation: &Participation) -> bool {
    participation.policy.gate
        && sampled(
            input,
            &participation.policy.classification.behavior_fingerprint,
            &participation.policy.classification.observation_revision,
            participation
                .policy
                .classification
                .observation
                .gate_sample_rate,
        )
}

pub(super) fn submit(
    input: &Input,
    participation: &Participation,
    observation: &Observation,
    replay: ReferenceReplay,
) {
    let policy = &participation.policy.classification.observation;
    if !selected(input, participation) {
        return;
    }
    let job = match Job::admit(
        input,
        &participation.revision,
        policy,
        replay.bytes,
        replay.cost_reservation_microusd,
    ) {
        Ok(job) => job,
        Err(reason) => {
            observation.reference(reason, Some(false), None);
            return;
        },
    };
    let observation = observation.clone();
    observation.reference("queued", Some(false), None);
    let participation = participation.clone();
    let queue = Duration::from_millis(policy.queue_budget_ms);
    let inference = Duration::from_millis(policy.inference_budget_ms);
    tokio::spawn(async move {
        let queued = Instant::now();
        let permit = match tokio::time::timeout(queue, WORKER.clone().acquire_owned()).await {
            Ok(Ok(permit)) => permit,
            _ => {
                observation.reference("queue_expired", Some(false), None);
                return;
            },
        };
        if queued.elapsed() >= TTL {
            observation.reference("expired", Some(false), None);
            return;
        }
        if !participation.revalidate().await {
            observation.reference("stale_policy", Some(false), None);
            return;
        }
        let Some(remaining) = TTL.checked_sub(queued.elapsed()) else {
            observation.reference("expired", Some(false), None);
            return;
        };
        match tokio::time::timeout(remaining.min(inference), (replay.current)()).await {
            Ok(true) => {},
            Ok(false) => {
                observation.reference("source_or_reference_changed", Some(false), None);
                return;
            },
            Err(_) => {
                observation.reference("source_check_expired", Some(false), None);
                return;
            },
        }
        // A dispatch future can stop waiting while its physical provider call
        // continues. Keep both the single-worker permit and the retained-byte
        // admission until that call actually ends, even after our report
        // deadline expires. Dropping the JoinHandle detaches this bounded task.
        let run = replay.run;
        let mut physical = tokio::spawn(async move {
            let _permit = permit;
            let result = run().await;
            (result, job)
        });
        let result = tokio::time::timeout(inference, &mut physical).await;
        match result {
            Ok(Ok((result, _job))) => {
                let check_access = replay.access_current.as_ref().unwrap_or(&replay.current);
                let source_current = TTL.checked_sub(queued.elapsed()).map(|remaining| {
                    tokio::time::timeout(remaining.min(inference), check_access())
                });
                let source_current = match source_current {
                    Some(check) => matches!(check.await, Ok(true)),
                    None => false,
                };
                if !source_current || !participation.revalidate().await {
                    observation.reference(
                        "source_or_policy_changed_after_replay",
                        result.attempted,
                        receipt_without_labels(result.reference),
                    );
                    return;
                }
                observation.reference(result.status, result.attempted, result.reference);
            },
            Ok(Err(_)) => observation.reference("failed", None, None),
            Err(_) => {
                // The provider task is still running and owns both its model
                // permit and snapshot admission. Publish the deadline now,
                // then join the physical task for its eventual receipt. A
                // late answer is never comparison evidence or authority.
                observation.reference("inference_expired", None, None);
                tokio::spawn(async move {
                    let Ok((result, _job)) = physical.await else {
                        observation.reference("inference_expired_late_failed", None, None);
                        return;
                    };
                    let check_access = replay.access_current.as_ref().unwrap_or(&replay.current);
                    let source_current = tokio::time::timeout(inference, check_access())
                        .await
                        .is_ok_and(|current| current);
                    if !source_current || !participation.revalidate().await {
                        observation.reference(
                            "inference_expired_late_source_changed",
                            result.attempted,
                            receipt_without_labels(result.reference),
                        );
                        return;
                    }
                    let receipt = receipt_without_labels(result.reference);
                    observation.reference("inference_expired_late", result.attempted, receipt);
                });
            },
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use magicllm::{LlmScope, LlmTraceContext, LlmWorkloadClass};

    fn input(case: &str, scope: &str) -> Input {
        Input {
            operation: "memory_utility_review".into(),
            projection_version: "p".into(),
            reference_version: "r".into(),
            case_id: case.into(),
            context: None,
            items: vec![],
            required_questions: vec![],
            scope: LlmTraceContext::new(
                LlmScope::new(scope, "workspace"),
                LlmWorkloadClass::Ambient,
            ),
            agent: None,
            requires_completion: false,
            replay: None,
        }
    }

    #[test]
    fn snapshots_are_rejected_before_an_unbounded_copy() {
        let small: (String, usize) = snapshot_bounded(&"hello").unwrap();
        assert_eq!(small.0, "hello");
        assert!(snapshot_bounded::<_, String>(&"x".repeat(MAX_SNAPSHOT)).is_none());
    }

    #[test]
    fn selection_and_completed_case_dedup_are_deterministic_and_scoped() {
        let policy = ClassificationObservationPolicy::default();
        let first = input("case", "observer-a");
        assert!(!sampled(&first, "behavior", "sample-v1", 0.0));
        assert!(sampled(&first, "behavior", "sample-v1", 1.0));
        assert_eq!(
            sampled(&first, "behavior", "sample-v1", 0.25),
            sampled(&first, "behavior", "sample-v1", 0.25)
        );
        let job = Job::admit(&first, "revision", &policy, 10, Ok(100)).unwrap();
        assert!(matches!(
            Job::admit(&first, "revision", &policy, 10, Ok(100)),
            Err("duplicate")
        ));
        drop(job);
        assert!(matches!(
            Job::admit(&first, "revision", &policy, 10, Ok(100)),
            Err("completed_duplicate")
        ));
        let other = input("case", "observer-b");
        let _other_job = Job::admit(&other, "revision", &policy, 10, Ok(100)).unwrap();
    }

    #[test]
    fn failed_or_expired_admission_is_deduplicated() {
        let first = input("failed-case", "observer-failure");
        let policy = ClassificationObservationPolicy::default();
        // The job can leave the queue before a provider call. Its selected
        // input is still a terminal sample for this bounded retention window.
        drop(Job::admit(&first, "revision", &policy, 10, Ok(100)).unwrap());
        assert!(matches!(
            Job::admit(&first, "revision", &policy, 10, Ok(100)),
            Err("completed_duplicate")
        ));
    }

    #[test]
    fn changed_source_receipt_keeps_cost_but_never_labels() {
        let reference = Reference {
            labels: BTreeMap::from([(
                "0".into(),
                BTreeMap::from([("resolution".into(), serde_json::json!("replace_existing"))]),
            )]),
            call: Some(super::super::telemetry::ReferenceCall {
                receipt: None,
                provider: "fixture".into(),
                model: "fixture".into(),
                profile: None,
                operation: None,
                input_tokens: Some(10),
                output_tokens: Some(2),
                cache_read_tokens: Some(0),
                cache_write_tokens: Some(0),
                cost_usd: Some(0.001),
                pricing_version: None,
                attempts_complete: true,
            }),
            latency_ms: 40,
        };
        let retained = receipt_without_labels(Some(reference)).unwrap();
        assert!(retained.labels.is_empty());
        assert_eq!(retained.call.unwrap().cost_usd, Some(0.001));
        assert_eq!(retained.latency_ms, 40);
    }

    #[test]
    fn unpriced_or_over_budget_reference_work_is_never_admitted() {
        let first = input("cost-bound", "observer-cost");
        let mut policy = ClassificationObservationPolicy::default();
        policy.max_reserved_microusd_per_hour = 1;
        assert!(matches!(
            Job::admit(&first, "revision", &policy, 10, Err("cost_unknown")),
            Err("cost_unknown")
        ));
        assert!(matches!(
            Job::admit(&first, "revision", &policy, 10, Ok(2)),
            Err("spending_limit")
        ));
    }
}
