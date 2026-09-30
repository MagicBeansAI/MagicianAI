//! `VerificationPolicy` — what "checked" means for a project, and who gets to
//! decide it.
//!
//! ## Why per-command timeouts are policy, not a constant
//!
//! `run_project_checks` hard-codes a 240s per-command cap. This repository
//! checks in 5–7 minutes and tests in ~16, so a controller built on that
//! constant would turn every healthy build here into a verification failure.
//! The 60-minute turn and 2-hour task budgets elsewhere are irrelevant to a
//! 4-minute per-command cap. Timeouts therefore live on the [`CheckSpec`].
//!
//! ## Why inference is a fallback, not a policy
//!
//! `detect_check_commands` covers `package.json` scripts, `cargo
//! check/test/clippy` and pytest. It will not find this repository's
//! authoritative `make check-all`, and it knows nothing about the environment
//! a check needs — this repo requires `CARGO_TARGET_DIR` on the SSD, and
//! getting that wrong turns a 7-minute check into a 40-minute one. Inference
//! is what we do when nobody told us; it is not what we do instead of asking.
//!
//! ## Anti-weakening
//!
//! The policy must be protected from the agent it gates. A coding agent can
//! delete a test script from `package.json`, point it at `echo pass`, or edit
//! a repository-owned policy file to drop required checks. Without a rule,
//! verification becomes something the subject of verification can switch off.
//! So [`resolve`] enforces:
//!
//! * owner/project configuration is authoritative;
//! * repository inference **may add** checks and **may never remove** a
//!   baseline required check;
//! * a policy change takes effect on a **later** task, never on the task that
//!   made it — unless explicitly approved.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

/// A per-command timeout that is generous enough for a real repository.
///
/// Deliberately far above `run_project_checks`'s 240s: this is the *default*
/// for a check whose policy did not name one, and a default that fails healthy
/// builds is worse than no default.
pub const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 30 * 60;

/// Default ceiling for a whole policy run.
pub const DEFAULT_TOTAL_TIMEOUT_SECS: u64 = 90 * 60;

/// Bounded capture per stream. Full logs live in run artifacts.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// One check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckSpec {
    /// Stable identity, used to compare a resolved policy against its
    /// baseline. Two specs with the same `id` are the same check even if the
    /// command was edited — which is exactly how a weakening attempt is
    /// detected.
    pub id: String,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Human-readable form for logs and diagnostics.
    pub display: String,
    /// Per-command timeout. `None` means [`DEFAULT_COMMAND_TIMEOUT_SECS`].
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Environment this check needs. Ordered so the policy digest is stable.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Advisory checks run and are recorded, but never gate.
    #[serde(default)]
    pub advisory: bool,
}

impl CheckSpec {
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs.unwrap_or(DEFAULT_COMMAND_TIMEOUT_SECS))
    }

    pub fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            return Err(anyhow!("check spec id is empty"));
        }
        if self.program.trim().is_empty() {
            return Err(anyhow!("check spec {} has an empty program", self.id));
        }
        if self.timeout_secs == Some(0) {
            return Err(anyhow!(
                "check spec {} has a zero timeout; it could never pass",
                self.id
            ));
        }
        Ok(())
    }
}

/// Network posture for the sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
    /// No egress. The safe default — running a repository's checks is
    /// executing untrusted code.
    #[default]
    Denied,
    /// Loopback only, for checks that spin up a local server.
    Loopback,
    /// Full egress. Requires an explicit decision, because a check with
    /// network access can exfiltrate anything the sandbox can read.
    Allowed,
}

/// §4.9 — running a repository's checks is executing untrusted code. This
/// belongs in the runner, not in model instructions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxPolicy {
    pub network: NetworkPolicy,
    /// Whether the process environment may carry credentials. Default false:
    /// a check does not need the owner's API keys to compile code.
    pub allow_secrets: bool,
    /// Paths (relative to the ephemeral workspace) a check may write to
    /// besides the workspace itself — build output roots, caches.
    pub writable_roots: Vec<String>,
    /// Absolute paths mounted for cache reuse. Kept separate from
    /// `writable_roots` because these live outside the workspace and are the
    /// difference between a 7-minute check and a 40-minute one.
    pub cache_mounts: Vec<String>,
    pub max_output_bytes: usize,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self {
            network: NetworkPolicy::default(),
            allow_secrets: false,
            writable_roots: Vec::new(),
            cache_mounts: Vec::new(),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }
}

/// Where a policy came from. Authority decreases down the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicySource {
    /// Configured by the owner outside the repository. Highest authority: the
    /// agent being gated cannot edit it.
    Owner,
    /// Committed in the repository. Authoritative, but editable by the agent —
    /// so changes are subject to the later-task rule.
    Project,
    /// Derived by `detect_check_commands`. Lowest authority; may only add.
    Inferred,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationPolicy {
    pub source: PolicySource,
    #[serde(default)]
    pub required: Vec<CheckSpec>,
    #[serde(default)]
    pub advisory: Vec<CheckSpec>,
    #[serde(default)]
    pub total_timeout_secs: Option<u64>,
    #[serde(default)]
    pub sandbox: SandboxPolicy,
    /// Identifies the authoritative owner/project policy this was resolved
    /// against, for audit.
    #[serde(default)]
    pub baseline_ref: Option<String>,
}

impl VerificationPolicy {
    pub fn empty(source: PolicySource) -> Self {
        Self {
            source,
            required: Vec::new(),
            advisory: Vec::new(),
            total_timeout_secs: None,
            sandbox: SandboxPolicy::default(),
            baseline_ref: None,
        }
    }

    pub fn total_timeout(&self) -> Duration {
        Duration::from_secs(
            self.total_timeout_secs
                .unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECS),
        )
    }

    /// True when there is nothing to gate on. Drives the `unverified` outcome,
    /// which is explicit and never a silent pass.
    pub fn has_required_checks(&self) -> bool {
        !self.required.is_empty()
    }

    pub fn validate(&self) -> Result<()> {
        let mut seen = BTreeSet::new();
        for spec in self.required.iter().chain(self.advisory.iter()) {
            spec.validate()?;
            if !seen.insert(spec.id.as_str()) {
                return Err(anyhow!("duplicate check id {}", spec.id));
            }
        }
        if self.total_timeout_secs == Some(0) {
            return Err(anyhow!("policy total timeout is zero"));
        }
        Ok(())
    }
}

/// A policy that has been through [`resolve`] and is safe to execute.
///
/// Distinct from [`VerificationPolicy`] on purpose: only a `ResolvedPolicy`
/// can produce an attestation the gate will accept, so it is impossible to
/// accidentally verify against an unresolved (and therefore un-anti-weakened)
/// policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedPolicy {
    pub required: Vec<CheckSpec>,
    pub advisory: Vec<CheckSpec>,
    pub total_timeout_secs: u64,
    pub sandbox: SandboxPolicy,
    pub baseline_ref: Option<String>,
    /// Checks the repository tried to remove or weaken and that were kept
    /// anyway. Surfaced rather than silently corrected — a project that keeps
    /// trying to drop its own test suite is information the owner wants.
    pub refused_weakenings: Vec<String>,
    /// Additions the repository contributed on top of the baseline.
    pub added_by_repository: Vec<String>,
}

impl ResolvedPolicy {
    pub fn total_timeout(&self) -> Duration {
        Duration::from_secs(self.total_timeout_secs)
    }

    pub fn has_required_checks(&self) -> bool {
        !self.required.is_empty()
    }

    /// Content address of the entire resolved policy.
    ///
    /// Part of the attestation key, so evidence produced under one policy can
    /// never satisfy a gate resolved under another — including the case where
    /// only a timeout or an env var differs.
    pub fn digest(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        let mut field = |name: &str, value: &str| {
            hasher.update(name.as_bytes());
            hasher.update(b"\x1f");
            hasher.update(value.as_bytes());
            hasher.update(b"\x1e");
        };
        for (label, specs) in [("required", &self.required), ("advisory", &self.advisory)] {
            for spec in specs {
                field(label, &spec.id);
                field("program", &spec.program);
                for arg in &spec.args {
                    field("arg", arg);
                }
                field("timeout", &spec.timeout().as_secs().to_string());
                for (k, v) in &spec.env {
                    field("env", k);
                    field("env_value", v);
                }
                field("advisory", if spec.advisory { "1" } else { "0" });
            }
        }
        field("total_timeout", &self.total_timeout_secs.to_string());
        field("network", &format!("{:?}", self.sandbox.network));
        field(
            "allow_secrets",
            if self.sandbox.allow_secrets { "1" } else { "0" },
        );
        for root in &self.sandbox.writable_roots {
            field("writable_root", root);
        }
        for mount in &self.sandbox.cache_mounts {
            field("cache_mount", mount);
        }
        hasher.finalize().to_hex().to_string()
    }
}

/// Resolve the policy that will actually run.
///
/// `baseline` is the authoritative owner/project policy as pinned *before*
/// this task began. `repository` is what the repository says now — which the
/// agent under verification may have edited during this very task.
/// `inferred` is the `detect_check_commands` fallback.
///
/// The later-task rule falls out of the parameterisation: callers pass the
/// pinned baseline, so a policy edit made during this task can only ever be
/// visible through `repository`, where additions are honoured and removals
/// are not.
pub fn resolve(
    baseline: Option<&VerificationPolicy>,
    repository: Option<&VerificationPolicy>,
    inferred: &[CheckSpec],
) -> Result<ResolvedPolicy> {
    if let Some(b) = baseline {
        b.validate()?;
    }
    if let Some(r) = repository {
        r.validate()?;
    }

    // Start from the baseline's required set. These can never be removed.
    let mut required: Vec<CheckSpec> = baseline.map(|b| b.required.clone()).unwrap_or_default();
    let baseline_ids: BTreeSet<String> = required.iter().map(|s| s.id.clone()).collect();
    let mut refused_weakenings = Vec::new();
    let mut added_by_repository = Vec::new();

    if let Some(repo) = repository {
        let repo_ids: BTreeSet<&str> = repo.required.iter().map(|s| s.id.as_str()).collect();

        // A baseline check the repository dropped entirely.
        for id in &baseline_ids {
            if !repo_ids.contains(id.as_str()) {
                refused_weakenings.push(id.clone());
            }
        }

        for spec in &repo.required {
            match required.iter_mut().find(|existing| existing.id == spec.id) {
                Some(existing) => {
                    // The check exists in the baseline. The repository may
                    // *strengthen* it — a shorter timeout is not weakening,
                    // and neither is extra env — but it may not swap the
                    // command out for something that always passes.
                    if existing.program != spec.program || existing.args != spec.args {
                        refused_weakenings.push(spec.id.clone());
                    } else {
                        existing.env.extend(spec.env.clone());
                        if let Some(t) = spec.timeout_secs {
                            existing.timeout_secs = Some(t);
                        }
                    }
                },
                None => {
                    // A brand-new required check. Additions are always
                    // welcome.
                    required.push(spec.clone());
                    added_by_repository.push(spec.id.clone());
                },
            }
        }
    }

    // Inference only fills gaps, and only when it contributes something new.
    for spec in inferred {
        if !required.iter().any(|existing| existing.id == spec.id) {
            let mut spec = spec.clone();
            spec.advisory = false;
            added_by_repository.push(spec.id.clone());
            required.push(spec);
        }
    }

    let advisory: Vec<CheckSpec> = baseline
        .map(|b| b.advisory.clone())
        .unwrap_or_default()
        .into_iter()
        .chain(repository.map(|r| r.advisory.clone()).unwrap_or_default())
        .filter(|spec| !required.iter().any(|r| r.id == spec.id))
        .collect();

    // The strictest configured total timeout wins.
    let total_timeout_secs = [
        baseline.and_then(|b| b.total_timeout_secs),
        repository.and_then(|r| r.total_timeout_secs),
    ]
    .into_iter()
    .flatten()
    .min()
    .unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECS);

    // Sandbox comes from the highest-authority source that specified one. A
    // repository must not be able to grant itself network access or secrets.
    let sandbox = baseline
        .map(|b| b.sandbox.clone())
        .unwrap_or_else(SandboxPolicy::default);

    refused_weakenings.sort();
    refused_weakenings.dedup();
    added_by_repository.sort();
    added_by_repository.dedup();

    let resolved = ResolvedPolicy {
        required,
        advisory,
        total_timeout_secs,
        sandbox,
        baseline_ref: baseline.and_then(|b| b.baseline_ref.clone()),
        refused_weakenings,
        added_by_repository,
    };
    Ok(resolved)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn spec(id: &str, program: &str) -> CheckSpec {
        CheckSpec {
            id: id.into(),
            program: program.into(),
            args: vec!["run".into()],
            display: format!("{program} run"),
            timeout_secs: None,
            env: BTreeMap::new(),
            advisory: false,
        }
    }

    fn baseline_with(required: Vec<CheckSpec>) -> VerificationPolicy {
        VerificationPolicy {
            source: PolicySource::Owner,
            required,
            advisory: Vec::new(),
            total_timeout_secs: None,
            sandbox: SandboxPolicy::default(),
            baseline_ref: Some("owner-policy-v1".into()),
        }
    }

    #[test]
    fn default_per_command_timeout_is_generous_enough_for_a_real_repo() {
        // The 240s constant in run_project_checks would fail this repo's own
        // `make check-all`. The default here must not.
        assert!(DEFAULT_COMMAND_TIMEOUT_SECS >= 7 * 60);
        assert_eq!(
            spec("a", "make").timeout().as_secs(),
            DEFAULT_COMMAND_TIMEOUT_SECS
        );
    }

    #[test]
    fn policy_supplied_timeout_overrides_the_default() {
        let mut s = spec("check", "make");
        s.timeout_secs = Some(20 * 60);
        assert_eq!(s.timeout(), Duration::from_secs(1200));
    }

    #[test]
    fn a_repository_cannot_remove_a_baseline_required_check() {
        let baseline = baseline_with(vec![spec("test", "make"), spec("lint", "make")]);
        // The repository drops `test` entirely.
        let repo = VerificationPolicy {
            source: PolicySource::Project,
            required: vec![spec("lint", "make")],
            ..VerificationPolicy::empty(PolicySource::Project)
        };

        let resolved = resolve(Some(&baseline), Some(&repo), &[]).unwrap();
        let ids: Vec<&str> = resolved.required.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"test"), "baseline check must survive removal");
        assert_eq!(resolved.refused_weakenings, vec!["test".to_string()]);
    }

    #[test]
    fn a_repository_cannot_swap_a_required_check_for_echo_pass() {
        let baseline = baseline_with(vec![spec("test", "make")]);
        let mut weakened = spec("test", "echo");
        weakened.args = vec!["pass".into()];
        let repo = VerificationPolicy {
            source: PolicySource::Project,
            required: vec![weakened],
            ..VerificationPolicy::empty(PolicySource::Project)
        };

        let resolved = resolve(Some(&baseline), Some(&repo), &[]).unwrap();
        let kept = resolved.required.iter().find(|s| s.id == "test").unwrap();
        assert_eq!(kept.program, "make", "the baseline command must be kept");
        assert_eq!(resolved.refused_weakenings, vec!["test".to_string()]);
    }

    #[test]
    fn a_repository_may_add_required_checks() {
        let baseline = baseline_with(vec![spec("test", "make")]);
        let repo = VerificationPolicy {
            source: PolicySource::Project,
            required: vec![spec("test", "make"), spec("audit", "cargo")],
            ..VerificationPolicy::empty(PolicySource::Project)
        };

        let resolved = resolve(Some(&baseline), Some(&repo), &[]).unwrap();
        let ids: Vec<&str> = resolved.required.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"audit"));
        assert!(resolved.refused_weakenings.is_empty());
        assert_eq!(resolved.added_by_repository, vec!["audit".to_string()]);
    }

    #[test]
    fn a_repository_may_strengthen_env_and_timeout_on_a_baseline_check() {
        let baseline = baseline_with(vec![spec("test", "make")]);
        let mut tightened = spec("test", "make");
        tightened.timeout_secs = Some(600);
        tightened
            .env
            .insert("CARGO_TARGET_DIR".into(), "/Volumes/build/builds".into());
        let repo = VerificationPolicy {
            source: PolicySource::Project,
            required: vec![tightened],
            ..VerificationPolicy::empty(PolicySource::Project)
        };

        let resolved = resolve(Some(&baseline), Some(&repo), &[]).unwrap();
        let kept = resolved.required.iter().find(|s| s.id == "test").unwrap();
        assert_eq!(kept.timeout_secs, Some(600));
        assert_eq!(
            kept.env.get("CARGO_TARGET_DIR").map(String::as_str),
            Some("/Volumes/build/builds")
        );
        assert!(resolved.refused_weakenings.is_empty());
    }

    #[test]
    fn inference_only_fills_gaps_and_never_replaces_a_baseline_check() {
        let baseline = baseline_with(vec![spec("test", "make")]);
        // Inference proposes its own `test` (cargo test) plus a new `lint`.
        let inferred = vec![spec("test", "cargo"), spec("lint", "cargo")];

        let resolved = resolve(Some(&baseline), None, &inferred).unwrap();
        let test = resolved.required.iter().find(|s| s.id == "test").unwrap();
        assert_eq!(
            test.program, "make",
            "inference must not override the owner"
        );
        assert!(resolved.required.iter().any(|s| s.id == "lint"));
    }

    #[test]
    fn a_repository_cannot_grant_itself_network_or_secrets() {
        let mut baseline = baseline_with(vec![spec("test", "make")]);
        baseline.sandbox.network = NetworkPolicy::Denied;
        baseline.sandbox.allow_secrets = false;

        let mut permissive = VerificationPolicy::empty(PolicySource::Project);
        permissive.sandbox.network = NetworkPolicy::Allowed;
        permissive.sandbox.allow_secrets = true;

        let resolved = resolve(Some(&baseline), Some(&permissive), &[]).unwrap();
        assert_eq!(resolved.sandbox.network, NetworkPolicy::Denied);
        assert!(!resolved.sandbox.allow_secrets);
    }

    #[test]
    fn the_strictest_total_timeout_wins() {
        let mut baseline = baseline_with(vec![spec("test", "make")]);
        baseline.total_timeout_secs = Some(3600);
        let mut repo = VerificationPolicy::empty(PolicySource::Project);
        repo.total_timeout_secs = Some(600);

        let resolved = resolve(Some(&baseline), Some(&repo), &[]).unwrap();
        assert_eq!(resolved.total_timeout_secs, 600);
    }

    #[test]
    fn sandbox_denies_network_and_secrets_by_default() {
        let s = SandboxPolicy::default();
        assert_eq!(s.network, NetworkPolicy::Denied);
        assert!(!s.allow_secrets);
    }

    #[test]
    fn no_required_checks_is_representable_and_not_a_pass() {
        let resolved = resolve(None, None, &[]).unwrap();
        assert!(!resolved.has_required_checks());
        // The gate turns this into `unverified`; nothing here claims success.
        assert!(resolved.required.is_empty());
    }

    #[test]
    fn policy_digest_is_stable_and_sensitive_to_every_component() {
        let baseline = baseline_with(vec![spec("test", "make")]);
        let a = resolve(Some(&baseline), None, &[]).unwrap();
        assert_eq!(
            a.digest(),
            resolve(Some(&baseline), None, &[]).unwrap().digest()
        );

        // A timeout-only change must move the digest — otherwise evidence
        // produced under a laxer policy could satisfy a stricter gate.
        let mut tighter = baseline.clone();
        tighter.required[0].timeout_secs = Some(60);
        assert_ne!(
            a.digest(),
            resolve(Some(&tighter), None, &[]).unwrap().digest()
        );

        // So must an env-only change.
        let mut with_env = baseline.clone();
        with_env.required[0]
            .env
            .insert("CARGO_TARGET_DIR".into(), "/tmp".into());
        assert_ne!(
            a.digest(),
            resolve(Some(&with_env), None, &[]).unwrap().digest()
        );

        // And a sandbox change.
        let mut networked = baseline;
        networked.sandbox.network = NetworkPolicy::Allowed;
        assert_ne!(
            a.digest(),
            resolve(Some(&networked), None, &[]).unwrap().digest()
        );
    }

    #[test]
    fn duplicate_and_degenerate_specs_are_rejected() {
        let mut p = baseline_with(vec![spec("test", "make"), spec("test", "cargo")]);
        assert!(p.validate().is_err(), "duplicate ids must be refused");

        p = baseline_with(vec![CheckSpec {
            timeout_secs: Some(0),
            ..spec("test", "make")
        }]);
        assert!(p.validate().is_err(), "a zero timeout could never pass");

        p = baseline_with(vec![CheckSpec {
            program: "  ".into(),
            ..spec("test", "make")
        }]);
        assert!(p.validate().is_err());
    }
}
