//! Whether a lane is safe to offer a Run button for.
//!
//! # Why this fails closed
//!
//! The two mistakes are not symmetric. Refusing to offer a lane that would in
//! fact have worked costs a page reload. Offering a lane whose service we could
//! not reach starts a run that dies partway through — after it has already
//! spent minutes of model time, or real provider money on a `provider_keys`
//! lane. So [`lane_readiness`] counts a requirement as satisfied only when a
//! probe explicitly said [`ProbeResult::Up`]. A requirement that could not be
//! determined, and one that was never probed at all, both block.
//!
//! # Why `Unknown` is a variant rather than a `bool`
//!
//! "I could not tell" and "it is definitely down" are different claims, and
//! only the second is a fact. Collapsing them would make the page state
//! something it does not know — "Ollama is down" when the truth is that a probe
//! timed out. They block identically, so the distinction costs nothing at the
//! gate; it is there so the *explanation* the page renders is true.
//!
//! Consequently a probe only ever returns [`ProbeResult::Down`] on evidence the
//! service itself produced: an HTTP response that was not a success, a binary
//! that is present but not executable, an environment with no configured key in
//! it. Every transport error and every timeout is [`ProbeResult::Unknown`].
//!
//! # Why the decision is separated from the probing
//!
//! [`lane_readiness`] is pure and takes a snapshot, so the fail-closed rule can
//! be table-tested without a network. The live probes fill that snapshot at the
//! edge and hold every decision they make in a small pure helper for the same
//! reason.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::registry::EvalRequirement;
use magician::config::MagicianConfig;

/// How long any one probe may take. Readiness is computed on page load, so the
/// whole set has to finish inside a request the user is waiting on — the probes
/// run concurrently, so this is close to the total budget, not a per-probe tax.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Where `run-ollama.sh` puts the generation daemon when nothing overrides it
/// (`scripts/run-ollama.sh:41`). Only a fallback: the configured endpoint wins.
const DEFAULT_OLLAMA_BASE_URL: &str = "http://127.0.0.1:11434";

/// The backend URL every live lane in the Makefile defaults to — see
/// `MAGICIAN_BASE_URL` in `scripts/run-live-evals-with-report.sh` and
/// `OBSERVABLE_SOURCES_LIVE_API_BASE_URL` / `STORAGE_GOVERNANCE_LIVE_API_BASE_URL`
/// in the Makefile. The probe must aim at whatever the lane will aim at, so this
/// deliberately mirrors those defaults instead of introspecting our own bind
/// address: a lane pointed at a different server is exactly the case where
/// "magician is obviously up, I am answering this request" would be a lie.
const DEFAULT_MAGICIAN_BASE_URL: &str = "http://127.0.0.1:3002";

/// `MAGICIAN_BIN ?= magician.bin` (`Makefile:319`), resolved against the repo
/// root the way the recipes write it (`./$(MAGICIAN_BIN)`).
const DEFAULT_MAGICIAN_BIN: &str = "magician.bin";

/// What a probe learned about one requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeResult {
    /// The service answered and said it was healthy.
    Up,
    /// Established as unavailable by evidence the service itself produced — an
    /// unsuccessful HTTP response, a missing or non-executable binary, an
    /// environment with none of the configured keys in it.
    Down,
    /// Could not be determined — treated as NOT ready. Every transport error and
    /// every timeout lands here, because a probe that failed to complete has
    /// learned nothing about the service.
    Unknown,
}

/// What every probe found, one entry per requirement that was actually probed.
///
/// A requirement missing from `results` is NOT the same as an empty
/// requirement: [`lane_readiness`] treats absence as unproven and therefore
/// blocking, so a probe that was skipped or panicked cannot silently pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeSnapshot {
    pub results: HashMap<EvalRequirement, ProbeResult>,
}

impl ProbeSnapshot {
    /// The recorded result, or [`ProbeResult::Unknown`] when this requirement
    /// was never probed. There is no "absent means fine" reading.
    pub fn result(&self, requirement: EvalRequirement) -> ProbeResult {
        self.results
            .get(&requirement)
            .copied()
            .unwrap_or(ProbeResult::Unknown)
    }

    /// Builder form, for tests and for assembling a snapshot probe by probe.
    #[must_use]
    pub fn with(mut self, requirement: EvalRequirement, result: ProbeResult) -> Self {
        self.results.insert(requirement, result);
        self
    }
}

/// Whether one lane may be offered, and what is standing in the way.
///
/// `missing` is the page's explanation: it names every unsatisfied requirement,
/// not just the first, so "start Ollama" is never followed by "…and now start
/// Magicutor" on the next reload. Pair it with the [`ProbeSnapshot`] to say
/// whether each one is down or merely unproven — both block, but only one of
/// them is a fact worth printing as one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneReadiness {
    pub ready: bool,
    pub missing: Vec<EvalRequirement>,
}

/// Fail-closed: ONLY an explicitly [`ProbeResult::Up`] requirement counts as
/// satisfied. [`ProbeResult::Unknown`] and absent from the snapshot both block.
///
/// A lane with no requirements is ready, which is the whole point of declaring
/// them: `kind=harness` lanes are self-contained and must not be gated behind a
/// service they never touch.
pub fn lane_readiness(requires: &[EvalRequirement], snapshot: &ProbeSnapshot) -> LaneReadiness {
    let mut missing: Vec<EvalRequirement> = requires
        .iter()
        .copied()
        .filter(|requirement| snapshot.result(*requirement) != ProbeResult::Up)
        .collect();
    missing.sort_unstable();
    missing.dedup();
    LaneReadiness {
        ready: missing.is_empty(),
        missing,
    }
}

/// Where each probe looks, resolved once per page load from config and the
/// environment so that no probe carries an endpoint literal of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeTargets {
    /// The Ollama generation daemon (`:11434` by convention). Held separately
    /// from the embedding daemon because `run-ollama.sh` starts and verifies two
    /// distinct daemons and the lanes use both.
    pub ollama_generation_base_url: String,
    /// The dedicated Ollama embedding daemon (`runtime.ollama.embedding_base_url`,
    /// `:11435` by default). `test-memory-temperature-live-eval` fails without
    /// it just as surely as without generation, so a coarse `requires=ollama`
    /// has to mean both.
    pub ollama_embedding_base_url: String,
    /// The running backend the live lanes drive.
    pub magician_base_url: String,
    /// `execution.magicutor_base_url` — a different process on a different port.
    pub magicutor_base_url: String,
    /// `./$(MAGICIAN_BIN)` under the repo root.
    pub magician_binary: PathBuf,
    /// The env var names the configured cloud profiles read their key from.
    /// Empty means the config declared none, which is unprovable rather than
    /// false, so [`probe_provider_keys`] answers `Unknown`.
    pub provider_key_envs: Vec<String>,
}

impl ProbeTargets {
    /// Resolves every endpoint from config, the ambient environment, and the
    /// repo's conventional defaults, in that order of preference.
    pub fn resolve(config: &MagicianConfig, repo_root: &Path) -> Self {
        Self::resolve_with(config, repo_root, |name| std::env::var(name).ok())
    }

    /// [`ProbeTargets::resolve`] with the environment injected, so the
    /// resolution rules can be tested without an ambient `MAGICIAN_BASE_URL`
    /// deciding whether the suite passes.
    pub fn resolve_with(
        config: &MagicianConfig,
        repo_root: &Path,
        env: impl Fn(&str) -> Option<String>,
    ) -> Self {
        // `MAGICIAN_OLLAMA_URL` is the override `run-ollama.sh` honours first;
        // `MAGICIAN_OLLAMA_BASE_URL` is already folded into the config by
        // `apply_runtime_service_endpoint_overrides`, so reading the profile
        // covers it.
        let ollama_generation_base_url = non_empty(env("MAGICIAN_OLLAMA_URL"))
            .or_else(|| configured_ollama_generation_base_url(config))
            .unwrap_or_else(|| DEFAULT_OLLAMA_BASE_URL.to_string());

        let magician_binary = repo_root.join(
            non_empty(env("MAGICIAN_BIN")).unwrap_or_else(|| DEFAULT_MAGICIAN_BIN.to_string()),
        );

        Self {
            ollama_generation_base_url: trim_base_url(&ollama_generation_base_url),
            ollama_embedding_base_url: trim_base_url(&config.runtime.ollama.embedding_base_url),
            magician_base_url: trim_base_url(
                &non_empty(env("MAGICIAN_BASE_URL"))
                    .unwrap_or_else(|| DEFAULT_MAGICIAN_BASE_URL.to_string()),
            ),
            magicutor_base_url: trim_base_url(&config.execution.magicutor_base_url),
            magician_binary,
            provider_key_envs: configured_provider_key_envs(config),
        }
    }
}

/// Probes every requirement kind concurrently and returns what was found.
///
/// Every requirement is present in the snapshot afterwards, including the ones
/// that could not be determined — an entry that says `Unknown` is legible on the
/// page, whereas a missing entry is only legible as a blocked lane with no
/// stated reason.
pub async fn probe_all(targets: &ProbeTargets) -> ProbeSnapshot {
    let (ollama, magician, magicutor) = tokio::join!(
        probe_ollama(
            &targets.ollama_generation_base_url,
            &targets.ollama_embedding_base_url
        ),
        probe_magician(&targets.magician_base_url),
        probe_magicutor(&targets.magicutor_base_url),
    );

    ProbeSnapshot::default()
        .with(EvalRequirement::Ollama, ollama)
        .with(EvalRequirement::Magician, magician)
        .with(EvalRequirement::Magicutor, magicutor)
        .with(
            EvalRequirement::MagicianBinary,
            probe_magician_binary(&targets.magician_binary),
        )
        .with(
            EvalRequirement::ProviderKeys,
            probe_provider_keys(&targets.provider_key_envs),
        )
}

/// Both Ollama daemons, via the same `/api/tags` liveness call `run-ollama.sh`
/// uses (`scripts/run-ollama.sh:136-145`).
pub async fn probe_ollama(generation_base_url: &str, embedding_base_url: &str) -> ProbeResult {
    // Bound to locals rather than built inline: `tokio::join!` holds both
    // futures across an await, so a `&format!(...)` temporary would be dropped
    // while the future it was handed to is still borrowing it.
    let generation_url = format!("{}/api/tags", trim_base_url(generation_base_url));
    let embedding_url = format!("{}/api/tags", trim_base_url(embedding_base_url));
    let (generation, embedding) =
        tokio::join!(probe_http(&generation_url), probe_http(&embedding_url));
    worst_of([generation, embedding])
}

/// The running backend, via the aggregated `GET /health` it already serves
/// (`magician_api::service_health_api`).
pub async fn probe_magician(base_url: &str) -> ProbeResult {
    probe_http(&format!("{}/health", trim_base_url(base_url))).await
}

/// Magicutor serves its own `/health` — the same endpoint magician's aggregated
/// health handler probes, so there is exactly one answer to "is Magicutor up".
pub async fn probe_magicutor(base_url: &str) -> ProbeResult {
    probe_http(&format!("{}/health", trim_base_url(base_url))).await
}

/// Whether `./$(MAGICIAN_BIN)` is on disk and runnable.
///
/// Absent or not executable is [`ProbeResult::Down`]: `make build-all-release`
/// has not been run, which is a fact the page can act on. Only a stat that
/// failed for some *other* reason is unproven.
pub fn probe_magician_binary(path: &Path) -> ProbeResult {
    match std::fs::metadata(path) {
        Ok(metadata) => binary_result(metadata.is_file(), executable_bit(&metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ProbeResult::Down,
        Err(_) => ProbeResult::Unknown,
    }
}

/// Whether real provider credentials are configured, judged against the env var
/// names the configured cloud profiles actually name.
pub fn probe_provider_keys(declared: &[String]) -> ProbeResult {
    provider_keys_result(declared, |name| std::env::var(name).ok())
}

/// GET `url`, mapping any transport failure to [`ProbeResult::Unknown`].
async fn probe_http(url: &str) -> ProbeResult {
    match reqwest::Client::new()
        .get(url)
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
    {
        Ok(response) => http_result(Some(response.status().as_u16())),
        // Timed out, refused, DNS, TLS — the probe did not complete, so it
        // learned nothing. `None` is "no response", never "definitely down".
        Err(_) => http_result(None),
    }
}

/// The whole HTTP decision, in one place with no I/O in it.
fn http_result(status: Option<u16>) -> ProbeResult {
    match status {
        Some(code) if (200..300).contains(&code) => ProbeResult::Up,
        // The service answered and did not claim health. That answer is
        // evidence, so it is the one HTTP case that may say `Down`.
        Some(_) => ProbeResult::Down,
        None => ProbeResult::Unknown,
    }
}

/// The whole binary decision. `executable` is `None` on platforms with no
/// execute bit to read, which is unproven rather than false.
fn binary_result(is_file: bool, executable: Option<bool>) -> ProbeResult {
    match (is_file, executable) {
        // A directory (or a socket) at that path is not the binary.
        (false, _) => ProbeResult::Down,
        (true, Some(true)) => ProbeResult::Up,
        (true, Some(false)) => ProbeResult::Down,
        (true, None) => ProbeResult::Unknown,
    }
}

/// The whole provider-credential decision.
///
/// `declared` empty means the config named no key-bearing profile, so there is
/// nothing to look up and no basis for a claim either way — [`ProbeResult::Unknown`],
/// which blocks. Any one configured key being present reads as `Up`: the
/// `requires=provider_keys` token is coarse and cannot say *which* provider a
/// lane will bill, so this cannot promise more than "this machine has cloud
/// credentials".
fn provider_keys_result(
    declared: &[String],
    lookup: impl Fn(&str) -> Option<String>,
) -> ProbeResult {
    if declared.is_empty() {
        return ProbeResult::Unknown;
    }
    let any_present = declared
        .iter()
        .any(|name| lookup(name).is_some_and(|value| !value.trim().is_empty()));
    if any_present {
        ProbeResult::Up
    } else {
        ProbeResult::Down
    }
}

/// Combines sub-probes of one requirement, fail-closed and evidence-first:
/// anything definitively `Down` makes the requirement `Down`, anything unproven
/// makes it `Unknown`, and only an all-`Up` set is `Up`. An empty set proves
/// nothing.
fn worst_of(results: impl IntoIterator<Item = ProbeResult>) -> ProbeResult {
    let mut combined: Option<ProbeResult> = None;
    for result in results {
        combined = Some(match (combined, result) {
            (Some(ProbeResult::Down), _) | (_, ProbeResult::Down) => ProbeResult::Down,
            (Some(ProbeResult::Unknown), _) | (_, ProbeResult::Unknown) => ProbeResult::Unknown,
            _ => ProbeResult::Up,
        });
    }
    combined.unwrap_or(ProbeResult::Unknown)
}

#[cfg(unix)]
fn executable_bit(metadata: &std::fs::Metadata) -> Option<bool> {
    use std::os::unix::fs::PermissionsExt;
    Some(metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable_bit(_metadata: &std::fs::Metadata) -> Option<bool> {
    None
}

/// The generation endpoint of the first configured Ollama profile, with the
/// `/api/generate` suffix the router stores stripped back off — every Ollama
/// profile in the shipped config writes it that way
/// (`magician-config.yaml:1384`), and `/api/tags` hangs off the base.
fn configured_ollama_generation_base_url(config: &MagicianConfig) -> Option<String> {
    let router = config.router_config()?;
    let mut endpoints: Vec<&str> = router
        .profiles
        .values()
        .filter(|profile| profile.provider == magicllm::capability::LLMProviderKind::Ollama)
        .filter_map(|profile| profile.api_base_url.as_deref())
        .map(str::trim)
        .filter(|endpoint| !endpoint.is_empty())
        .collect();
    // `profiles` is a HashMap, so pick deterministically rather than by
    // whichever iteration order this build happened to produce.
    endpoints.sort_unstable();
    endpoints.dedup();
    let endpoint: &str = *endpoints.first()?;
    Some(trim_base_url(
        endpoint.strip_suffix("/api/generate").unwrap_or(endpoint),
    ))
}

/// The env var names the configured non-Ollama profiles read their key from.
///
/// Only profiles that *declare* `api_key_env` count. `magicllm::bootstrap`
/// falls back to a per-provider default when it is absent, but mirroring that
/// table here would be a second copy of it that rots silently — and guessing
/// wrong in the "no key needed" direction is the expensive one.
fn configured_provider_key_envs(config: &MagicianConfig) -> Vec<String> {
    let Some(router) = config.router_config() else {
        return Vec::new();
    };
    let mut names: Vec<String> = router
        .profiles
        .values()
        .filter(|profile| profile.provider != magicllm::capability::LLMProviderKind::Ollama)
        .filter_map(|profile| profile.api_key_env.as_deref())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

fn trim_base_url(value: &str) -> String {
    value.trim().trim_end_matches('/').to_string()
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(entries: &[(EvalRequirement, ProbeResult)]) -> ProbeSnapshot {
        let mut snapshot = ProbeSnapshot::default();
        for (requirement, result) in entries {
            snapshot = snapshot.with(*requirement, *result);
        }
        snapshot
    }

    #[test]
    fn a_lane_with_no_requirements_is_always_ready() {
        // Not even an empty snapshot may block a harness lane: it declared that
        // it needs nothing, and gating it on a service it never touches would
        // make `kind=harness` meaningless.
        let readiness = lane_readiness(&[], &ProbeSnapshot::default());
        assert!(readiness.ready);
        assert!(readiness.missing.is_empty());
    }

    #[test]
    fn a_satisfied_requirement_is_ready() {
        let readiness = lane_readiness(
            &[EvalRequirement::Ollama],
            &snapshot(&[(EvalRequirement::Ollama, ProbeResult::Up)]),
        );
        assert!(readiness.ready);
        assert!(readiness.missing.is_empty());
    }

    #[test]
    fn a_down_requirement_blocks_and_names_itself() {
        let readiness = lane_readiness(
            &[EvalRequirement::Magicutor],
            &snapshot(&[(EvalRequirement::Magicutor, ProbeResult::Down)]),
        );
        assert!(!readiness.ready);
        assert_eq!(readiness.missing, vec![EvalRequirement::Magicutor]);
    }

    /// FAIL CLOSED: an unprobeable service must never read as ready. Offering a
    /// Run button on a service we could not reach is how an expensive lane dies
    /// halfway through.
    #[test]
    fn an_unprobeable_requirement_is_not_ready() {
        let readiness = lane_readiness(
            &[EvalRequirement::ProviderKeys],
            &snapshot(&[(EvalRequirement::ProviderKeys, ProbeResult::Unknown)]),
        );
        assert!(!readiness.ready);
        assert_eq!(readiness.missing, vec![EvalRequirement::ProviderKeys]);
    }

    #[test]
    fn a_requirement_absent_from_the_snapshot_is_not_ready() {
        // A probe that was skipped, or that panicked before recording anything,
        // leaves a hole. The hole must not read as satisfied.
        let readiness = lane_readiness(
            &[EvalRequirement::Magician],
            &snapshot(&[(EvalRequirement::Ollama, ProbeResult::Up)]),
        );
        assert!(!readiness.ready);
        assert_eq!(readiness.missing, vec![EvalRequirement::Magician]);
    }

    #[test]
    fn every_missing_requirement_is_reported_not_just_the_first() {
        let readiness = lane_readiness(
            &[
                EvalRequirement::Ollama,
                EvalRequirement::Magician,
                EvalRequirement::ProviderKeys,
            ],
            &snapshot(&[
                (EvalRequirement::Ollama, ProbeResult::Down),
                (EvalRequirement::Magician, ProbeResult::Up),
                (EvalRequirement::ProviderKeys, ProbeResult::Down),
            ]),
        );
        assert!(!readiness.ready);
        assert_eq!(
            readiness.missing,
            vec![EvalRequirement::Ollama, EvalRequirement::ProviderKeys]
        );
    }

    /// The real umbrella lane: `test-live-evals` declares all five. One down
    /// service must not hide the other four from whoever is trying to fix them.
    #[test]
    fn the_umbrella_lane_reports_all_five_when_nothing_is_up() {
        let requires = [
            EvalRequirement::Ollama,
            EvalRequirement::Magician,
            EvalRequirement::MagicianBinary,
            EvalRequirement::Magicutor,
            EvalRequirement::ProviderKeys,
        ];
        let readiness = lane_readiness(&requires, &ProbeSnapshot::default());
        assert!(!readiness.ready);
        assert_eq!(readiness.missing, requires.to_vec());
    }

    /// The page renders `missing` to explain the block, so it has to survive
    /// the wire.
    #[test]
    fn readiness_serializes_its_missing_requirements() {
        let readiness = lane_readiness(
            &[EvalRequirement::MagicianBinary],
            &snapshot(&[(EvalRequirement::MagicianBinary, ProbeResult::Down)]),
        );
        let json = serde_json::to_value(&readiness).unwrap();
        assert_eq!(json["ready"], false);
        assert_eq!(json["missing"][0], "magician_binary");
    }

    /// The snapshot is what tells the page whether a blocker is a fact or a
    /// shrug, so it has to survive the wire too — enum keys and all.
    #[test]
    fn a_snapshot_round_trips_through_json() {
        let original = snapshot(&[
            (EvalRequirement::Magicutor, ProbeResult::Unknown),
            (EvalRequirement::Ollama, ProbeResult::Up),
        ]);
        let json = serde_json::to_string(&original).unwrap();
        assert!(json.contains("magicutor"), "{json}");
        assert!(json.contains("unknown"), "{json}");
        let restored: ProbeSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn only_a_successful_response_reads_as_up() {
        assert_eq!(http_result(Some(200)), ProbeResult::Up);
        assert_eq!(http_result(Some(204)), ProbeResult::Up);
        // The service answered and refused to claim health — that is evidence.
        assert_eq!(http_result(Some(404)), ProbeResult::Down);
        assert_eq!(http_result(Some(503)), ProbeResult::Down);
    }

    /// A timeout is not a diagnosis. This is the rule that keeps `Down` a claim
    /// we can defend.
    #[test]
    fn a_transport_failure_is_unknown_never_down() {
        assert_eq!(http_result(None), ProbeResult::Unknown);
    }

    #[test]
    fn a_missing_or_unexecutable_binary_is_down_but_an_unreadable_mode_is_unknown() {
        assert_eq!(binary_result(true, Some(true)), ProbeResult::Up);
        assert_eq!(binary_result(true, Some(false)), ProbeResult::Down);
        assert_eq!(binary_result(false, Some(true)), ProbeResult::Down);
        assert_eq!(binary_result(true, None), ProbeResult::Unknown);
    }

    #[test]
    fn the_binary_probe_reads_the_execute_bit_off_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("magician.bin");

        // Nothing built yet.
        assert_eq!(probe_magician_binary(&path), ProbeResult::Down);

        std::fs::write(&path, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Present but not runnable is still a fact, and still blocking.
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(probe_magician_binary(&path), ProbeResult::Down);

            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(probe_magician_binary(&path), ProbeResult::Up);
        }

        // A directory where the binary should be is not the binary.
        assert_eq!(probe_magician_binary(dir.path()), ProbeResult::Down);
    }

    #[test]
    fn provider_keys_need_a_declared_name_and_a_non_empty_value() {
        let declared = vec!["OPENAI_API_KEY".to_string(), "GEMINI_API_KEY".to_string()];

        assert_eq!(
            provider_keys_result(&declared, |name| (name == "GEMINI_API_KEY")
                .then(|| "sk-real".to_string())),
            ProbeResult::Up
        );
        assert_eq!(provider_keys_result(&declared, |_| None), ProbeResult::Down);
        // A key exported as whitespace is not a credential.
        assert_eq!(
            provider_keys_result(&declared, |_| Some("   ".to_string())),
            ProbeResult::Down
        );
    }

    /// With no key-bearing profile configured there is nothing to look up, so
    /// there is no basis for saying credentials are absent — and no basis for
    /// offering the lane either.
    #[test]
    fn provider_keys_with_nothing_declared_are_unknown_not_down() {
        assert_eq!(provider_keys_result(&[], |_| None), ProbeResult::Unknown);
    }

    #[test]
    fn combining_sub_probes_prefers_evidence_then_fails_closed() {
        use ProbeResult::{Down, Unknown, Up};
        assert_eq!(worst_of([Up, Up]), Up);
        assert_eq!(worst_of([Up, Down]), Down);
        assert_eq!(worst_of([Up, Unknown]), Unknown);
        // A definite failure outranks an unproven one: it is the more useful
        // thing to tell the operator, and both block regardless.
        assert_eq!(worst_of([Unknown, Down]), Down);
        // Nothing probed proves nothing.
        assert_eq!(worst_of([]), Unknown);
    }

    #[test]
    fn targets_fall_back_to_the_repo_conventions_when_nothing_is_configured() {
        let config = MagicianConfig::default();
        let targets = ProbeTargets::resolve_with(&config, Path::new("/repo"), |_| None);

        assert_eq!(targets.magician_base_url, DEFAULT_MAGICIAN_BASE_URL);
        assert_eq!(targets.ollama_generation_base_url, DEFAULT_OLLAMA_BASE_URL);
        assert_eq!(targets.magician_binary, Path::new("/repo/magician.bin"));
        // The embedding daemon is always configured; it has a serde default.
        assert!(!targets.ollama_embedding_base_url.is_empty());
        assert!(!targets.magicutor_base_url.is_empty());
    }

    /// The lane aims at `MAGICIAN_BASE_URL`; so must the probe. Probing our own
    /// bind address instead would report "ready" for a lane pointed somewhere
    /// else entirely.
    #[test]
    fn the_environment_redirects_the_probes_the_same_way_it_redirects_the_lanes() {
        let config = MagicianConfig::default();
        let targets = ProbeTargets::resolve_with(&config, Path::new("/repo"), |name| match name {
            "MAGICIAN_BASE_URL" => Some("http://10.0.0.4:9999/".to_string()),
            "MAGICIAN_OLLAMA_URL" => Some("http://10.0.0.5:11434".to_string()),
            "MAGICIAN_BIN" => Some("magician.debug.bin".to_string()),
            _ => None,
        });

        // Trailing slashes are stripped so `{base}/health` never doubles up.
        assert_eq!(targets.magician_base_url, "http://10.0.0.4:9999");
        assert_eq!(targets.ollama_generation_base_url, "http://10.0.0.5:11434");
        assert_eq!(
            targets.magician_binary,
            Path::new("/repo/magician.debug.bin")
        );
    }

    /// `LLMProfile` has no `Default`, and spelling out its 16 fields would make
    /// every future field addition break these tests for no reason. Its own
    /// serde defaults are the stable way to build a fixture.
    fn profile(fields: serde_json::Value) -> magicllm::config::LLMProfile {
        serde_json::from_value(fields).expect("fixture profile deserializes")
    }

    fn router_with(
        profiles: Vec<(&str, magicllm::config::LLMProfile)>,
    ) -> magicllm::config::LLMRouterConfig {
        let mut router = magicllm::config::LLMRouterConfig::default();
        for (name, profile) in profiles {
            router.profiles.insert(name.to_string(), profile);
        }
        router
    }

    /// The router stores Ollama endpoints as `.../api/generate`
    /// (`magician-config.yaml:1384`), but liveness hangs off the base.
    #[test]
    fn a_configured_ollama_profile_is_reduced_to_its_base_url() {
        let mut config = MagicianConfig::default();
        config.llm.router = Some(router_with(vec![(
            "local",
            profile(serde_json::json!({
                "provider": "ollama",
                "model": "qwen3",
                "api_base_url": "http://ollama.internal:11434/api/generate",
            })),
        )]));

        let targets = ProbeTargets::resolve_with(&config, Path::new("/repo"), |_| None);
        assert_eq!(
            targets.ollama_generation_base_url,
            "http://ollama.internal:11434"
        );
    }

    /// Ollama profiles carry no key, so counting them would make every machine
    /// look credentialed.
    #[test]
    fn only_cloud_profiles_contribute_provider_key_names() {
        let mut config = MagicianConfig::default();
        config.llm.router = Some(router_with(vec![
            (
                "local",
                profile(serde_json::json!({
                    "provider": "ollama",
                    "model": "qwen3",
                    "api_key_env": "OLLAMA_API_KEY",
                })),
            ),
            (
                "cloud",
                profile(serde_json::json!({
                    "provider": "openai",
                    "model": "gpt-5",
                    "api_key_env": "OPENAI_API_KEY",
                })),
            ),
        ]));

        let targets = ProbeTargets::resolve_with(&config, Path::new("/repo"), |_| None);
        assert_eq!(
            targets.provider_key_envs,
            vec!["OPENAI_API_KEY".to_string()]
        );
    }
}
