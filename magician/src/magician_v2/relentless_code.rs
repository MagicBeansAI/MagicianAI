//! Sealed code artifacts checked by the existing verification runner.
use super::execution::verification::{
    attestation::{AttemptOutcome, CommandResult},
    policy::{NetworkPolicy, ResolvedPolicy},
    runner::{RunnerEnv, VerificationRunner},
    snapshot::{EphemeralWorkspace, SourceSnapshot},
};
use anyhow::{ensure, Context, Result};
use magician_relentless::{review::Outcome, store::RunStore};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationRecipe {
    #[serde(default)]
    pub runner_version: u32,
    pub policy: ResolvedPolicy,
    pub read_only_roots: Vec<PathBuf>,
    pub declared_outputs: Vec<String>,
    #[serde(default)]
    pub executable_hashes: BTreeMap<String, String>,
    #[serde(default)]
    pub launcher_hash: String,
    #[serde(default)]
    pub protected_files: BTreeMap<String, String>,
    pub maximum_result_bytes: u64,
}

impl VerificationRecipe {
    pub fn resolve_environment(&mut self) -> Result<()> {
        if self.runner_version == 0 {
            self.runner_version = super::execution::verification::runner::RUNNER_VERSION;
        }
        let launcher = super::execution::verification::confined::launcher_pin()?.1;
        if self.launcher_hash.is_empty() {
            self.launcher_hash = launcher;
        }
        if self.executable_hashes.is_empty() {
            self.executable_hashes = self.current_executables()?;
        }
        for root in &mut self.read_only_roots {
            *root = root.canonicalize()?;
        }
        self.validate()
    }
    fn current_executables(&self) -> Result<BTreeMap<String, String>> {
        self.policy
            .required
            .iter()
            .chain(&self.policy.advisory)
            .map(|spec| {
                spec.validate()?;
                let program = runtime_core::process::resolve_program_str(
                    &spec.program,
                    spec.env.get("PATH").map(String::as_str),
                )
                .canonicalize()?;
                Ok((
                    spec.id.clone(),
                    blake3::hash(&std::fs::read(program)?).to_hex().to_string(),
                ))
            })
            .collect()
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.runner_version == super::execution::verification::runner::RUNNER_VERSION,
            "verification runner semantics changed; review a new contract"
        );
        ensure!(
            self.policy.has_required_checks()
                && self.policy.total_timeout_secs > 0
                && self.policy.sandbox.max_output_bytes > 0
                && self.maximum_result_bytes > 0
                && self.maximum_result_bytes < u64::MAX,
            "verification needs required checks and finite time/output limits"
        );
        ensure!(
            matches!(self.policy.sandbox.network, NetworkPolicy::Denied)
                && !self.policy.sandbox.allow_secrets
                && self.policy.sandbox.cache_mounts.is_empty()
                && self.policy.sandbox.writable_roots.is_empty(),
            "verification must remain confined to its workspace without network or secrets"
        );
        ensure!(
            self.launcher_hash == super::execution::verification::confined::launcher_pin()?.1
                && self.executable_hashes == self.current_executables()?,
            "verification executable identity changed"
        );
        let mut ids = std::collections::BTreeSet::new();
        for spec in self.policy.required.iter().chain(&self.policy.advisory) {
            ensure!(ids.insert(&spec.id), "duplicate verification check ID");
        }
        for root in &self.read_only_roots {
            ensure!(
                root.is_absolute() && root.canonicalize()? == *root && root.parent().is_some(),
                "verification read root is not an exact bounded path"
            );
        }
        for file in self
            .protected_files
            .keys()
            .chain(self.declared_outputs.iter())
        {
            ensure!(
                !file.is_empty()
                    && std::path::Path::new(file)
                        .components()
                        .all(|c| matches!(c, std::path::Component::Normal(_))),
                "verification file/output path is invalid"
            );
        }
        ensure!(
            self.protected_files
                .values()
                .all(|hash| magician_relentless::contract::valid_hash(hash)),
            "unresolved protected source hash"
        );
        Ok(())
    }
    pub fn hash(&self) -> Result<String> {
        self.validate()?;
        self.frozen_hash()
    }
    pub fn frozen_hash(&self) -> Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(&(
            "relentless-code-verification-v1",
            self,
        ))?)
        .to_hex()
        .to_string())
    }
}

pub fn sealed_evidence(
    recipe: &VerificationRecipe,
    artifact_hash: &str,
    store: &RunStore,
    run_id: uuid::Uuid,
) -> Result<Option<CodeReviewEvidence>> {
    let recipe_hash = recipe.frozen_hash()?;
    let key = blake3::hash(&serde_json::to_vec(&("code-review-v1", &recipe_hash))?)
        .to_hex()
        .to_string();
    let Some(cache) = store.prepared_evidence(run_id, artifact_hash, &key)? else {
        return Ok(None);
    };
    let bytes = cache
        .get("verification.json")
        .context("sealed verification evidence missing")?;
    ensure!(
        bytes.len() as u64 <= recipe.maximum_result_bytes,
        "verification evidence exceeds recipe"
    );
    let evidence: CodeReviewEvidence = serde_json::from_slice(bytes)?;
    ensure!(
        evidence.artifact_hash == artifact_hash && evidence.recipe_hash == recipe_hash,
        "verification evidence binding changed"
    );
    Ok(Some(evidence))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeReviewEvidence {
    pub artifact_hash: String,
    pub recipe_hash: String,
    pub snapshot_hash: String,
    pub policy_hash: String,
    pub runner_hash: String,
    pub outcome: Outcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub results: Vec<CommandResult>,
    pub mutations: Vec<String>,
    pub protected_changes: Vec<String>,
}

fn jobs() -> &'static Mutex<BTreeMap<(String, uuid::Uuid), CancellationToken>> {
    static JOBS: OnceLock<Mutex<BTreeMap<(String, uuid::Uuid), CancellationToken>>> =
        OnceLock::new();
    JOBS.get_or_init(Default::default)
}
fn drained() -> &'static tokio::sync::Notify {
    static DRAINED: OnceLock<tokio::sync::Notify> = OnceLock::new();
    DRAINED.get_or_init(Default::default)
}
struct OwnedReview {
    key: (String, uuid::Uuid),
}
impl Drop for OwnedReview {
    fn drop(&mut self) {
        jobs()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
        drained().notify_waiters();
    }
}
pub async fn quiesce(owner: &str) {
    loop {
        let notification = drained().notified();
        let tokens: Vec<_> = jobs()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|((current, _), _)| current == owner)
            .map(|(_, token)| token.clone())
            .collect();
        if tokens.is_empty() {
            return;
        }
        for token in tokens {
            token.cancel();
        }
        notification.await;
    }
}

pub async fn review(
    owner: &str,
    holder: &str,
    operation: &magician_relentless::budget::OperationIntent,
    recipe: &VerificationRecipe,
    files: &BTreeMap<String, Vec<u8>>,
    artifact_hash: &str,
    store: Arc<RunStore>,
    run_id: uuid::Uuid,
    deadline: chrono::DateTime<chrono::Utc>,
    cancel: CancellationToken,
) -> Result<CodeReviewEvidence> {
    let recipe_hash = recipe.hash()?;
    let key = blake3::hash(&serde_json::to_vec(&("code-review-v1", &recipe_hash))?)
        .to_hex()
        .to_string();
    let reader = store.clone();
    let artifact = artifact_hash.to_owned();
    let identity = key.clone();
    let (cache, previously_started) = tokio::task::spawn_blocking(move || {
        Ok::<_, anyhow::Error>((
            reader.prepared_evidence(run_id, &artifact, &identity)?,
            reader.evidence_preparation_started(run_id, &artifact, &identity)?,
        ))
    })
    .await??;
    if let Some(cache) = cache {
        let bytes = cache
            .get("verification.json")
            .context("sealed verification evidence missing")?;
        ensure!(
            bytes.len() as u64 <= recipe.maximum_result_bytes,
            "verification evidence exceeds recipe"
        );
        let evidence: CodeReviewEvidence = serde_json::from_slice(bytes)?;
        ensure!(
            evidence.artifact_hash == artifact_hash && evidence.recipe_hash == recipe_hash,
            "verification evidence binding changed"
        );
        return Ok(evidence);
    }
    ensure!(
        !cancel.is_cancelled() && deadline > chrono::Utc::now(),
        "verification window closed"
    );
    let protected_changes: Vec<_> = recipe
        .protected_files
        .iter()
        .filter(|(file, hash)| {
            !files
                .get(*file)
                .is_some_and(|bytes| blake3::hash(bytes).to_hex().as_str() == hash.as_str())
        })
        .map(|(file, _)| file.clone())
        .collect();
    let scratch = Arc::new(tempfile::tempdir()?);
    let source = scratch.path().join("source");
    std::fs::create_dir(&source)?;
    for (file, bytes) in files {
        ensure!(
            std::path::Path::new(file)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
            "code artifact path is invalid"
        );
        let path = source.join(file);
        std::fs::create_dir_all(path.parent().context("code file parent missing")?)?;
        std::fs::write(path, bytes)?;
    }
    let snapshot = SourceSnapshot::capture_exact(&source)?;
    // Snapshot exclusions are conveniences in ordinary verification. A
    // declared artifact file must never silently disappear from this check.
    ensure!(
        snapshot.entries.len() == files.len()
            && files.keys().all(|file| snapshot.entries.contains_key(file)),
        "a declared code file was excluded from its verification snapshot"
    );
    let workspace = EphemeralWorkspace::materialise(
        snapshot,
        &source,
        scratch.path().join("checks"),
        recipe
            .declared_outputs
            .iter()
            .cloned()
            .chain([".verification-home".into()])
            .collect(),
    )?;
    let environment = RunnerEnv::new(recipe.executable_hashes.clone());
    let runner_hash = environment.digest();
    let snapshot_hash = workspace.snapshot().digest();
    let policy_hash = recipe.policy.digest();
    let mut policy = recipe.policy.clone();
    policy.total_timeout_secs = policy
        .total_timeout_secs
        .min(u64::try_from((deadline - chrono::Utc::now()).num_seconds()).unwrap_or(0));
    ensure!(
        policy.total_timeout_secs > 0,
        "verification deadline reached"
    );
    let marker_store = store.clone();
    let marker_artifact = artifact_hash.to_owned();
    let marker_key = key.clone();
    let marker_holder = holder.to_owned();
    let marker_operation = operation.clone();
    let started = if previously_started {
        false
    } else {
        tokio::task::spawn_blocking(move || {
            marker_store.begin_evidence_preparation(
                run_id,
                &marker_holder,
                &marker_operation,
                &marker_artifact,
                &marker_key,
                chrono::Utc::now(),
            )
        })
        .await??
    };
    if !started {
        // A prior job may still publish its genuine result. Do not seal this
        // recovery observation over that result or dispatch checks again.
        return Ok(CodeReviewEvidence {
            artifact_hash: artifact_hash.to_owned(),
            recipe_hash,
            snapshot_hash,
            policy_hash,
            runner_hash,
            outcome: Outcome::Unknown,
            reason: Some(
                "Prior code verification has no sealed result; checks will not be resent".into(),
            ),
            results: vec![],
            mutations: vec![],
            protected_changes,
        });
    }
    let roots = recipe.read_only_roots.clone();
    let maximum_result_bytes = recipe.maximum_result_bytes;
    let artifact_hash = artifact_hash.to_owned();
    let cancel = cancel.child_token();
    ensure!(
        !cancel.is_cancelled(),
        "verification cancelled before job ownership"
    );
    let key_owned = (owner.to_owned(), uuid::Uuid::new_v4());
    jobs()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key_owned.clone(), cancel.clone());
    let owned = OwnedReview { key: key_owned };
    // Existing owned-job boundary keeps the scratch checkout and process
    // cleanup alive when a controller caller is cancelled/dropped.
    let _cancel_on_drop = cancel.clone().drop_guard();
    super::execution::runtime_boundary::spawn_execution_job(move || async move {
        let _scratch = scratch;
        let _owned = owned;
        let evidence = if !protected_changes.is_empty() {
            CodeReviewEvidence {
                artifact_hash,
                recipe_hash,
                snapshot_hash,
                policy_hash,
                runner_hash,
                outcome: Outcome::Fail,
                reason: None,
                results: vec![],
                mutations: vec![],
                protected_changes,
            }
        } else {
            let runner = VerificationRunner::new(environment)
                .with_cancellation(cancel)
                .with_confined_workspace(roots);
            let report = runner.run(&policy, workspace.path()).await?;
            let mutations = workspace.detect_source_mutations()?.tracked_mutations;
            let outcome = if !mutations.is_empty() {
                Outcome::Unknown
            } else {
                match report.outcome {
                    AttemptOutcome::Green => Outcome::Pass,
                    AttemptOutcome::Red => Outcome::Fail,
                    _ => Outcome::Unknown,
                }
            };
            CodeReviewEvidence {
                artifact_hash,
                recipe_hash,
                snapshot_hash,
                policy_hash,
                runner_hash,
                outcome,
                reason: None,
                results: report.results,
                mutations,
                protected_changes,
            }
        };
        let bytes = serde_json::to_vec(&evidence)?;
        ensure!(
            bytes.len() as u64 <= maximum_result_bytes,
            "verification result exceeds evidence ceiling"
        );
        let artifact = evidence.artifact_hash.clone();
        tokio::task::spawn_blocking(move || {
            store.seal_prepared_evidence(
                run_id,
                &artifact,
                &key,
                &BTreeMap::from([("verification.json".into(), bytes)]),
            )
        })
        .await??;
        Ok::<_, anyhow::Error>(evidence)
    })
    .await
    .context("owned verification job failed to join")?
}
