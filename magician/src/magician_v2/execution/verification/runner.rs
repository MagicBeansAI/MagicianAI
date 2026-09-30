//! The shared verification runner.
//!
//! One runner, two callers: this controller, and `run_project_checks` (which
//! becomes a caller rather than being replaced). That shared path is what
//! makes the advisory/authoritative distinction meaningful — an engineer may
//! still run checks during development, but only *this* runner, executing the
//! **entire resolved required policy** against the **exact gated snapshot**,
//! produces an attestation the gate will accept. Otherwise a targeted
//! `cargo test -p one-crate` could be reused as full verification.
//!
//! ## Running a repository's checks is executing untrusted code
//!
//! §4.9 puts network policy, secret exposure, write boundaries, child-process
//! cleanup and resource limits in the runner, not in model instructions. An
//! instruction is a request; this is a boundary. Concretely:
//!
//! * **Secrets** — the environment is cleared and rebuilt from an explicit
//!   allowlist. A check does not need the owner's API keys to compile code.
//! * **Child cleanup** — each command runs in its own process group, and a
//!   timeout terminates the *group*, not just the direct child. Killing only
//!   the child leaves the `cargo`/`node` grandchildren holding the CPU, which
//!   is how a "timed out" run keeps burning the machine.
//! * **Output** — streamed into a fixed-size tail as it arrives, so a check
//!   that prints a gigabyte of progress cannot exhaust memory or the evidence
//!   store. Capping only what is *stored* is not a bound: buffering the whole
//!   stream and truncating at the end holds every byte the check printed until
//!   it exits, which against a 30-minute timeout is gigabytes.
//! * **Total timeout** — bounds the whole policy, so N commands each just
//!   under their own limit cannot add up to an unbounded run.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::attestation::{AttemptOutcome, CommandResult};
use super::policy::{CheckSpec, NetworkPolicy, ResolvedPolicy};

/// Grace period between SIGTERM and SIGKILL for a timed-out process group.
const TERMINATION_GRACE: Duration = Duration::from_millis(500);

/// Environment variables always passed through, regardless of secret policy.
/// Deliberately tiny: everything a toolchain needs to find itself, and nothing
/// that identifies or authenticates the owner.
const ENV_ALLOWLIST: &[&str] = &["PATH", "HOME", "LANG", "LC_ALL", "TMPDIR", "SHELL", "USER"];

/// Identity of the runner and its toolchain.
///
/// Part of the attestation key, because reusing a green result after the
/// compiler, runtime or runner implementation changed would be unsafe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerEnv {
    /// Bumped whenever this runner's execution semantics change.
    pub runner_version: u32,
    /// Toolchain fingerprints the caller considers relevant — compiler
    /// version, node version, container image digest.
    pub toolchain: BTreeMap<String, String>,
}

/// Bumped when execution semantics change in a way that could alter a result.
pub const RUNNER_VERSION: u32 = 1;

impl RunnerEnv {
    pub fn new(toolchain: BTreeMap<String, String>) -> Self {
        Self {
            runner_version: RUNNER_VERSION,
            toolchain,
        }
    }

    pub fn digest(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"runner_version\x1f");
        hasher.update(self.runner_version.to_string().as_bytes());
        hasher.update(b"\x1e");
        for (k, v) in &self.toolchain {
            hasher.update(k.as_bytes());
            hasher.update(b"\x1f");
            hasher.update(v.as_bytes());
            hasher.update(b"\x1e");
        }
        hasher.finalize().to_hex().to_string()
    }
}

/// Why a run stopped early, when it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunTermination {
    /// Every command in the policy ran.
    Completed,
    /// The policy's total timeout elapsed. Remaining commands did not run, so
    /// the result is indeterminate rather than red — we do not know what the
    /// unrun commands would have said.
    TotalTimeout,
    /// A required command failed and the runner stopped early.
    FailedFast,
}

/// The outcome of executing one resolved policy once.
#[derive(Debug, Clone, PartialEq)]
pub struct RunReport {
    pub results: Vec<CommandResult>,
    pub termination: RunTermination,
    pub outcome: AttemptOutcome,
    pub elapsed: Duration,
}

/// Executes a resolved policy inside a prepared workspace.
pub struct VerificationRunner {
    env: RunnerEnv,
    /// Stop at the first failing required command. On by default: a repair
    /// loop wants the first real diagnostic quickly, and running a 16-minute
    /// test suite after `cargo check` already failed buys nothing.
    fail_fast: bool,
}

impl VerificationRunner {
    pub fn new(env: RunnerEnv) -> Self {
        Self {
            env,
            fail_fast: true,
        }
    }

    /// Run every command, even after a required failure. Useful when a human
    /// wants the whole picture rather than the first blocker.
    pub fn with_fail_fast(mut self, fail_fast: bool) -> Self {
        self.fail_fast = fail_fast;
        self
    }

    pub fn env(&self) -> &RunnerEnv {
        &self.env
    }

    /// Execute `policy` with `workspace` as the working directory.
    ///
    /// `workspace` must be the materialised ephemeral checkout, never the real
    /// workspace: checks write build output, and doing that in the developer's
    /// tree is how a verification run corrupts the thing it is verifying.
    pub async fn run(&self, policy: &ResolvedPolicy, workspace: &Path) -> Result<RunReport> {
        let started = Instant::now();
        let total_timeout = policy.total_timeout();
        let mut results: Vec<CommandResult> = Vec::new();
        let mut termination = RunTermination::Completed;

        // Required first, so fail-fast surfaces a gating failure before time
        // is spent on advisory work.
        let ordered: Vec<(&CheckSpec, bool)> = policy
            .required
            .iter()
            .map(|s| (s, false))
            .chain(policy.advisory.iter().map(|s| (s, true)))
            .collect();

        for (spec, advisory) in ordered {
            let remaining = total_timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                termination = RunTermination::TotalTimeout;
                break;
            }

            // A command never gets longer than what remains of the total.
            let budget = spec.timeout().min(remaining);
            // Whether the *total* budget, not the command's own, is what
            // bounded this run.
            let clipped_by_total = budget < spec.timeout();
            let result = self
                .run_one(spec, workspace, budget, advisory, policy)
                .await;

            // A command the total budget cut short did not fail — the run ran
            // out of time inside it. Attributing that to the check reports a
            // red on a build nobody finished testing, and only shows up when
            // the clock expires during the last command (otherwise the next
            // iteration sees `remaining == 0` and stops here anyway).
            if result.timed_out && clipped_by_total {
                results.push(result);
                termination = RunTermination::TotalTimeout;
                break;
            }

            let failed_required = !advisory && !result.passed();
            results.push(result);

            if failed_required && self.fail_fast {
                termination = RunTermination::FailedFast;
                break;
            }
        }

        // A total-timeout stop is indeterminate, not red: commands that never
        // ran are unknown, and calling that a failure would send a healthy
        // build into a repair loop.
        let outcome = match termination {
            RunTermination::TotalTimeout => AttemptOutcome::Indeterminate,
            _ => super::attestation::VerificationAttempt::outcome_from_required(&results),
        };

        Ok(RunReport {
            results,
            termination,
            outcome,
            elapsed: started.elapsed(),
        })
    }

    async fn run_one(
        &self,
        spec: &CheckSpec,
        workspace: &Path,
        budget: Duration,
        advisory: bool,
        policy: &ResolvedPolicy,
    ) -> CommandResult {
        let started = Instant::now();

        // The allowlist below re-sets PATH on the child (the process PATH),
        // and a policy-declared `PATH` in `spec.env` is applied last and
        // wins; a bare program plus either override would force std onto
        // `fork` instead of `posix_spawn`, so resolve it against the PATH
        // the child actually receives (see `runtime_core::process`).
        let program = runtime_core::process::resolve_program_str(
            spec.program.as_str(),
            spec.env.get("PATH").map(String::as_str),
        )
        .into_os_string();
        let args: Vec<std::ffi::OsString> = spec
            .args
            .iter()
            .map(|a| std::ffi::OsString::from(a.as_str()))
            .collect();

        // Reuse the existing OS sandbox wrapper so this runner inherits
        // whatever platform confinement the coding path already applies.
        let mut cmd =
            crate::magician_v2::execution::coding_engine::os_sandbox_command(&program, &args);

        cmd.current_dir(workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // Secrets: clear everything, then re-add only what is allowed.
        cmd.env_clear();
        for key in ENV_ALLOWLIST {
            if let Ok(value) = std::env::var(key) {
                cmd.env(key, value);
            }
        }
        if policy.sandbox.allow_secrets {
            // Explicitly opted in; inherit the ambient environment on top of
            // the allowlist.
            for (k, v) in std::env::vars() {
                cmd.env(k, v);
            }
        }
        // Policy-declared env last so it always wins.
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        // Deterministic, non-interactive output.
        cmd.env("CI", "true")
            .env("NO_COLOR", "1")
            .env("FORCE_COLOR", "0");
        if matches!(policy.sandbox.network, NetworkPolicy::Denied) {
            // Advisory to well-behaved tooling. Real enforcement belongs to
            // the OS sandbox; this stops the common case of a package manager
            // reaching for the network mid-check.
            cmd.env("NO_NETWORK", "1")
                .env("npm_config_offline", "true")
                .env("CARGO_NET_OFFLINE", "true");
        }

        // Own process group, so a timeout can take the whole tree down.
        #[cfg(unix)]
        cmd.process_group(0);

        // `mut`: the pipes are taken off it and `wait()` borrows it.
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(error) => {
                return CommandResult {
                    command: spec.display.clone(),
                    exit_code: None,
                    duration_ms: started.elapsed().as_millis() as u64,
                    timed_out: false,
                    stdout_tail: String::new(),
                    stderr_tail: format!("failed to spawn `{}`: {error}", spec.display),
                    advisory,
                };
            },
        };

        let pid = child.id();
        let cap = policy.sandbox.max_output_bytes;

        // Drain both pipes into fixed-size tails *while the check runs*.
        //
        // `wait_with_output` buffered the entire stream and truncated
        // afterwards, so the cap bounded what was *stored* and nothing bounded
        // what was *held*. A check printing a megabyte a second against the
        // 30-minute default reached ~1.8 GB resident before the timeout could
        // fire — and `cargo test` on this repository is already multi-megabyte
        // on a good day. The truncation now happens as the bytes arrive, so
        // resident output is O(cap) per stream regardless of how much the
        // check prints.
        let stdout = tokio::spawn(drain_to_tail(child.stdout.take(), cap));
        let stderr = tokio::spawn(drain_to_tail(child.stderr.take(), cap));

        // One deadline over the exit *and* the drain, as before. Bounding only
        // the exit would leave the collection unbounded whenever a grandchild
        // keeps the pipe open after its parent is gone.
        let collect = async {
            let status = child.wait().await?;
            let stdout = stdout.await.unwrap_or_else(|_| OutputTail::new(cap));
            let stderr = stderr.await.unwrap_or_else(|_| OutputTail::new(cap));
            Ok::<_, std::io::Error>((status, stdout, stderr))
        };

        match tokio::time::timeout(budget, collect).await {
            Ok(Ok((status, stdout, stderr))) => CommandResult {
                command: spec.display.clone(),
                exit_code: status.code(),
                duration_ms: started.elapsed().as_millis() as u64,
                timed_out: false,
                stdout_tail: scrub_workspace(&stdout.render(), workspace),
                stderr_tail: scrub_workspace(&stderr.render(), workspace),
                advisory,
            },
            Ok(Err(error)) => CommandResult {
                command: spec.display.clone(),
                exit_code: None,
                duration_ms: started.elapsed().as_millis() as u64,
                timed_out: false,
                stdout_tail: String::new(),
                stderr_tail: format!("error running `{}`: {error}", spec.display),
                advisory,
            },
            Err(_) => {
                // Reach for the group by pid rather than the child handle:
                // killing only the direct child would leave the build tool's
                // children running. The two drain tasks are left detached —
                // the group is gone, so both pipes hit EOF and each task ends
                // holding at most `cap` bytes.
                terminate_process_group(pid).await;
                CommandResult {
                    command: spec.display.clone(),
                    exit_code: None,
                    duration_ms: started.elapsed().as_millis() as u64,
                    timed_out: true,
                    stdout_tail: String::new(),
                    stderr_tail: format!(
                        "`{}` timed out after {}s",
                        spec.display,
                        budget.as_secs()
                    ),
                    advisory,
                }
            },
        }
    }
}

/// Token that stands in for the ephemeral checkout root in stored output.
pub const WORKSPACE_TOKEN: &str = "<workspace>";

/// Replace the ephemeral checkout root with a stable token.
///
/// Every pass materialises a *fresh* workspace under a new random directory,
/// and toolchains print absolute paths in their diagnostics. Storing those
/// paths verbatim means round N and round N+1 disagree on every line that
/// mentions a file, so the no-progress fingerprint never matches and a stuck
/// repair loop runs until its budget is gone instead of stopping at the
/// second identical failure. Scrubbing at capture time — rather than at
/// comparison time — is what makes the *stored* record comparable, which
/// matters because the prior round's fingerprint is rebuilt from its
/// persisted attestation long after its workspace is deleted.
///
/// Both the path as given and its canonical form are scrubbed: on macOS a
/// child process resolves the temp root through a symlink, so the output
/// names a different prefix than the one handed to `current_dir`.
fn scrub_workspace(text: &str, workspace: &Path) -> String {
    let mut out = text.to_string();
    let mut forms = vec![workspace.to_path_buf()];
    if let Ok(canonical) = workspace.canonicalize() {
        if canonical != workspace {
            forms.push(canonical);
        }
    }
    // Longest first, so a prefix never shadows the fuller path.
    forms.sort_by_key(|p| std::cmp::Reverse(p.as_os_str().len()));
    for form in forms {
        out = out.replace(&form.to_string_lossy().to_string(), WORKSPACE_TOKEN);
    }
    out
}

/// How much of a pipe is read at a time. Bounds the transient allocation on
/// top of the tail; it has nothing to do with how much output is kept.
const OUTPUT_READ_CHUNK_BYTES: usize = 64 * 1024;

/// The last `cap` bytes of a stream, plus how many bytes went past.
///
/// Keeps the *tail* rather than the head: the useful part of a failing build
/// log is at the end. The difference from the previous `tail_bytes(&whole, cap)`
/// is only *when* the truncation happens — the rendered string is identical,
/// including the truncation banner and its byte count — but the whole stream
/// no longer has to exist first.
///
/// Memory is bounded at `2 * cap + OUTPUT_READ_CHUNK_BYTES`: compaction is
/// deferred until the buffer is twice the cap so that trimming is amortised
/// O(1) per byte rather than a memmove per read.
struct OutputTail {
    cap: usize,
    buf: Vec<u8>,
    /// Every byte ever pushed, including the ones already dropped.
    total: usize,
}

impl OutputTail {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            buf: Vec::new(),
            total: 0,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.total = self.total.saturating_add(chunk.len());
        if self.cap == 0 {
            return;
        }
        if chunk.len() >= self.cap {
            // This chunk alone overwrites everything kept so far.
            self.buf.clear();
            self.buf.extend_from_slice(&chunk[chunk.len() - self.cap..]);
            return;
        }
        self.buf.extend_from_slice(chunk);
        if self.buf.len() > self.cap.saturating_mul(2) {
            let excess = self.buf.len() - self.cap;
            self.buf.drain(..excess);
        }
    }

    /// The stored tail, in exactly the shape the evidence store has always
    /// held: the bytes when nothing was dropped, and a banner naming the
    /// dropped count when something was.
    fn render(&self) -> String {
        let kept = &self.buf[self.buf.len().saturating_sub(self.cap)..];
        if self.total <= self.cap {
            return String::from_utf8_lossy(kept).to_string();
        }
        // Avoid slicing mid-codepoint by letting the lossy conversion handle it.
        format!(
            "…[{} bytes truncated]…\n{}",
            self.total - self.cap,
            String::from_utf8_lossy(kept)
        )
    }
}

/// Read a child pipe to EOF, keeping only its tail.
///
/// A read error ends the drain with whatever was collected: the command's exit
/// status is the verdict, and losing the last few lines of a log is not a
/// reason to fail a check that passed.
async fn drain_to_tail<R>(reader: Option<R>, cap: usize) -> OutputTail
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    use tokio::io::AsyncReadExt;

    let mut tail = OutputTail::new(cap);
    let Some(mut reader) = reader else {
        return tail;
    };
    let mut chunk = vec![0u8; OUTPUT_READ_CHUNK_BYTES];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => tail.push(&chunk[..read]),
        }
    }
    tail
}

/// SIGTERM the group, then SIGKILL what survives the grace period.
///
/// `async` because the grace period is a real wait: `std::thread::sleep` here
/// parked a tokio worker for the duration, on the timeout path — i.e. exactly
/// when the runtime is already dealing with a check that misbehaved.
#[cfg(unix)]
async fn terminate_process_group(pid: Option<u32>) {
    let Some(pid) = pid.and_then(|value| i32::try_from(value).ok()) else {
        return;
    };
    let process_group = -pid;
    // SAFETY: negative pids address only the isolated process group created
    // for this invocation via `process_group(0)`.
    let delivered = unsafe { libc::kill(process_group, libc::SIGTERM) } == 0;
    if !delivered {
        return;
    }
    tokio::time::sleep(TERMINATION_GRACE).await;
    // SAFETY: same owned process group; SIGKILL is the bounded backstop.
    unsafe {
        libc::kill(process_group, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
async fn terminate_process_group(_pid: Option<u32>) {}

/// Build the toolchain fingerprint for [`RunnerEnv`].
///
/// Best-effort: a toolchain we cannot fingerprint is recorded as `unknown`
/// rather than omitted, so the digest still changes if it later becomes
/// knowable.
pub async fn probe_toolchain(workspace: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, program, args) in [
        ("rustc", "rustc", vec!["--version"]),
        ("cargo", "cargo", vec!["--version"]),
        ("node", "node", vec!["--version"]),
        ("python3", "python3", vec!["--version"]),
    ] {
        let value = tokio::process::Command::new(program)
            .args(&args)
            .current_dir(workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output()
            .await
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        out.insert(name.to_string(), value);
    }
    out
}

/// Convenience for callers that already hold a workspace and just want the
/// standard runner.
pub async fn default_runner(workspace: &Path) -> Result<VerificationRunner> {
    let toolchain = probe_toolchain(workspace).await;
    Ok(VerificationRunner::new(RunnerEnv::new(toolchain)))
}

/// Read a check policy committed in a repository, if present.
///
/// Absence is not an error — most projects will not have one, which is exactly
/// why inference exists as a fallback.
pub fn read_repository_policy(
    workspace: &Path,
) -> Result<Option<super::policy::VerificationPolicy>> {
    for candidate in [".magician/verification.json", ".magician/verification.yaml"] {
        let path = workspace.join(candidate);
        if !path.is_file() {
            continue;
        }
        let bytes = std::fs::read(&path)
            .with_context(|| format!("read repository policy {}", path.display()))?;
        let parsed: super::policy::VerificationPolicy = if candidate.ends_with(".json") {
            serde_json::from_slice(&bytes)
                .with_context(|| format!("parse repository policy {}", path.display()))?
        } else {
            serde_yaml::from_slice(&bytes)
                .with_context(|| format!("parse repository policy {}", path.display()))?
        };
        parsed.validate()?;
        return Ok(Some(parsed));
    }
    Ok(None)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::verification::policy::{
        resolve, PolicySource, SandboxPolicy, VerificationPolicy,
    };

    fn spec(id: &str, program: &str, args: &[&str], timeout_secs: Option<u64>) -> CheckSpec {
        CheckSpec {
            id: id.into(),
            program: program.into(),
            args: args.iter().map(|s| (*s).to_string()).collect(),
            display: format!("{program} {}", args.join(" ")),
            timeout_secs,
            env: BTreeMap::new(),
            advisory: false,
        }
    }

    fn policy_with(required: Vec<CheckSpec>) -> ResolvedPolicy {
        let baseline = VerificationPolicy {
            source: PolicySource::Owner,
            required,
            advisory: Vec::new(),
            total_timeout_secs: Some(120),
            sandbox: SandboxPolicy::default(),
            baseline_ref: Some("test".into()),
        };
        resolve(Some(&baseline), None, &[]).unwrap()
    }

    fn runner() -> VerificationRunner {
        VerificationRunner::new(RunnerEnv::new(BTreeMap::new()))
    }

    #[tokio::test]
    async fn a_passing_required_command_is_green() {
        let ws = tempfile::tempdir().unwrap();
        let policy = policy_with(vec![spec("ok", "true", &[], Some(30))]);
        let report = runner().run(&policy, ws.path()).await.unwrap();

        assert_eq!(report.outcome, AttemptOutcome::Green);
        assert_eq!(report.termination, RunTermination::Completed);
        assert_eq!(report.results.len(), 1);
        assert!(report.results[0].passed());
    }

    #[tokio::test]
    async fn a_failing_required_command_is_red_and_stops_early() {
        let ws = tempfile::tempdir().unwrap();
        let policy = policy_with(vec![
            spec("fail", "false", &[], Some(30)),
            spec("never", "true", &[], Some(30)),
        ]);
        let report = runner().run(&policy, ws.path()).await.unwrap();

        assert_eq!(report.outcome, AttemptOutcome::Red);
        assert_eq!(report.termination, RunTermination::FailedFast);
        assert_eq!(report.results.len(), 1, "fail-fast must skip the rest");
    }

    #[tokio::test]
    async fn fail_fast_can_be_disabled_to_collect_the_whole_picture() {
        let ws = tempfile::tempdir().unwrap();
        let policy = policy_with(vec![
            spec("fail", "false", &[], Some(30)),
            spec("also", "true", &[], Some(30)),
        ]);
        let report = runner()
            .with_fail_fast(false)
            .run(&policy, ws.path())
            .await
            .unwrap();

        assert_eq!(report.outcome, AttemptOutcome::Red);
        assert_eq!(report.results.len(), 2);
    }

    #[tokio::test]
    async fn per_command_timeout_comes_from_policy_not_a_constant() {
        // The whole point of §3.2: a slow-but-healthy command must be allowed
        // to finish when policy says so, and killed when policy says so.
        let ws = tempfile::tempdir().unwrap();
        let policy = policy_with(vec![spec("slow", "sleep", &["5"], Some(1))]);
        let report = runner().run(&policy, ws.path()).await.unwrap();

        assert!(report.results[0].timed_out);
        assert_eq!(report.outcome, AttemptOutcome::Red);
        // And it did not wait the full 5s.
        assert!(report.elapsed < Duration::from_secs(4));
    }

    #[tokio::test]
    async fn a_command_slower_than_240s_is_not_killed_when_policy_allows_it() {
        // Guards the specific regression: the old 240s constant would have
        // failed this repo's own `make check-all`.
        let spec = spec("long", "sleep", &["0"], Some(30 * 60));
        assert!(spec.timeout() > Duration::from_secs(240));

        let ws = tempfile::tempdir().unwrap();
        let policy = policy_with(vec![spec]);
        let report = runner().run(&policy, ws.path()).await.unwrap();
        assert!(!report.results[0].timed_out);
        assert_eq!(report.outcome, AttemptOutcome::Green);
    }

    #[tokio::test]
    async fn total_timeout_bounds_the_whole_run_and_is_indeterminate_not_red() {
        let ws = tempfile::tempdir().unwrap();
        let baseline = VerificationPolicy {
            source: PolicySource::Owner,
            required: vec![
                spec("a", "sleep", &["2"], Some(30)),
                spec("b", "true", &[], Some(30)),
            ],
            advisory: Vec::new(),
            total_timeout_secs: Some(1),
            sandbox: SandboxPolicy::default(),
            baseline_ref: None,
        };
        let policy = resolve(Some(&baseline), None, &[]).unwrap();
        let report = runner().run(&policy, ws.path()).await.unwrap();

        // Commands that never ran are unknown. Calling that red would send a
        // healthy build into a repair loop.
        assert_eq!(report.outcome, AttemptOutcome::Indeterminate);
    }

    #[tokio::test]
    async fn the_ephemeral_workspace_path_never_reaches_stored_output() {
        // Two rounds run in two different workspaces. If the path survives
        // into the stored tail, every diagnostic line differs between rounds
        // and the no-progress fingerprint can never match.
        let mut tails = Vec::new();
        for _ in 0..2 {
            let ws = tempfile::tempdir().unwrap();
            let baseline = VerificationPolicy {
                source: PolicySource::Owner,
                required: vec![spec("where", "pwd", &[], Some(30))],
                advisory: Vec::new(),
                total_timeout_secs: Some(60),
                sandbox: SandboxPolicy::default(),
                baseline_ref: None,
            };
            let policy = resolve(Some(&baseline), None, &[]).unwrap();
            let report = runner().run(&policy, ws.path()).await.unwrap();
            let tail = report.results[0].stdout_tail.clone();
            assert!(tail.contains(WORKSPACE_TOKEN), "not scrubbed: {tail}");
            tails.push(tail);
        }
        assert_eq!(
            tails[0], tails[1],
            "the same command in two workspaces must produce the same record"
        );
    }

    #[tokio::test]
    async fn advisory_failures_never_gate() {
        let ws = tempfile::tempdir().unwrap();
        let baseline = VerificationPolicy {
            source: PolicySource::Owner,
            required: vec![spec("ok", "true", &[], Some(30))],
            advisory: vec![CheckSpec {
                advisory: true,
                ..spec("lint", "false", &[], Some(30))
            }],
            total_timeout_secs: Some(60),
            sandbox: SandboxPolicy::default(),
            baseline_ref: None,
        };
        let policy = resolve(Some(&baseline), None, &[]).unwrap();
        let report = runner().run(&policy, ws.path()).await.unwrap();

        assert_eq!(report.outcome, AttemptOutcome::Green);
        assert_eq!(report.results.len(), 2);
        assert!(report.results.iter().any(|r| r.advisory && !r.passed()));
    }

    #[tokio::test]
    async fn secrets_are_scrubbed_unless_policy_opts_in() {
        let ws = tempfile::tempdir().unwrap();
        std::env::set_var("MAGICIAN_TEST_FAKE_SECRET", "must-not-leak");

        // `sh -c 'test -z "$VAR"'` succeeds only when the var is absent.
        let check = spec(
            "no-secret",
            "sh",
            &["-c", "test -z \"$MAGICIAN_TEST_FAKE_SECRET\""],
            Some(30),
        );
        let policy = policy_with(vec![check]);
        let report = runner().run(&policy, ws.path()).await.unwrap();
        assert_eq!(
            report.outcome,
            AttemptOutcome::Green,
            "the check environment must not carry ambient secrets"
        );

        std::env::remove_var("MAGICIAN_TEST_FAKE_SECRET");
    }

    #[tokio::test]
    async fn policy_declared_env_reaches_the_command() {
        let ws = tempfile::tempdir().unwrap();
        let mut check = spec(
            "env",
            "sh",
            &["-c", "test \"$CARGO_TARGET_DIR\" = /volumes/ssd"],
            Some(30),
        );
        check
            .env
            .insert("CARGO_TARGET_DIR".into(), "/volumes/ssd".into());

        let policy = policy_with(vec![check]);
        let report = runner().run(&policy, ws.path()).await.unwrap();
        assert_eq!(report.outcome, AttemptOutcome::Green);
    }

    #[tokio::test]
    async fn commands_run_in_the_supplied_workspace() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join("marker.txt"), "x").unwrap();

        let policy = policy_with(vec![spec("cwd", "test", &["-f", "marker.txt"], Some(30))]);
        let report = runner().run(&policy, ws.path()).await.unwrap();
        assert_eq!(report.outcome, AttemptOutcome::Green);
    }

    #[tokio::test]
    async fn a_missing_program_is_a_recorded_failure_not_a_panic() {
        let ws = tempfile::tempdir().unwrap();
        let policy = policy_with(vec![spec(
            "missing",
            "definitely-not-a-real-program-xyz",
            &[],
            Some(10),
        )]);
        let report = runner().run(&policy, ws.path()).await.unwrap();

        assert_eq!(report.outcome, AttemptOutcome::Red);
        assert!(report.results[0].stderr_tail.contains("failed to spawn"));
    }

    #[tokio::test]
    async fn output_is_captured_with_a_hard_cap_keeping_the_tail() {
        let ws = tempfile::tempdir().unwrap();
        let mut check = spec("noisy", "sh", &["-c", "seq 1 200000"], Some(60));
        check.advisory = false;

        let baseline = VerificationPolicy {
            source: PolicySource::Owner,
            required: vec![check],
            advisory: Vec::new(),
            total_timeout_secs: Some(120),
            sandbox: SandboxPolicy {
                max_output_bytes: 1024,
                ..SandboxPolicy::default()
            },
            baseline_ref: None,
        };
        let policy = resolve(Some(&baseline), None, &[]).unwrap();
        let report = runner().run(&policy, ws.path()).await.unwrap();

        let tail = &report.results[0].stdout_tail;
        assert!(tail.contains("truncated"), "expected a truncation marker");
        // The end of a build log is the useful part.
        assert!(tail.trim_end().ends_with("200000"));
    }

    #[test]
    fn runner_env_digest_is_stable_and_toolchain_sensitive() {
        let a = RunnerEnv::new(BTreeMap::from([("rustc".into(), "1.80".into())]));
        let b = RunnerEnv::new(BTreeMap::from([("rustc".into(), "1.80".into())]));
        assert_eq!(a.digest(), b.digest());

        // A compiler upgrade must invalidate reuse of a green result.
        let c = RunnerEnv::new(BTreeMap::from([("rustc".into(), "1.81".into())]));
        assert_ne!(a.digest(), c.digest());
    }

    #[test]
    fn an_absent_repository_policy_is_not_an_error() {
        let ws = tempfile::tempdir().unwrap();
        assert!(read_repository_policy(ws.path()).unwrap().is_none());
    }

    #[test]
    fn a_repository_policy_is_read_and_validated() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ws.path().join(".magician")).unwrap();
        std::fs::write(
            ws.path().join(".magician/verification.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "source": "project",
                "required": [{
                    "id": "test",
                    "program": "make",
                    "args": ["check-all"],
                    "display": "make check-all",
                    "timeout_secs": 1800
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        let parsed = read_repository_policy(ws.path()).unwrap().unwrap();
        assert_eq!(parsed.required.len(), 1);
        assert_eq!(parsed.required[0].timeout_secs, Some(1800));
    }

    #[test]
    fn a_malformed_repository_policy_is_an_error_not_a_silent_skip() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ws.path().join(".magician")).unwrap();
        std::fs::write(ws.path().join(".magician/verification.json"), b"{ nope").unwrap();

        // Fail closed: an unreadable policy is `unavailable`, never "no
        // checks configured".
        assert!(read_repository_policy(ws.path()).is_err());
    }

    /// What the previous whole-buffer-then-truncate produced, so the
    /// streaming tail can be held to it byte for byte.
    fn truncate_after_the_fact(bytes: &[u8], max: usize) -> String {
        if bytes.len() <= max {
            return String::from_utf8_lossy(bytes).to_string();
        }
        let start = bytes.len() - max;
        format!(
            "…[{} bytes truncated]…\n{}",
            start,
            String::from_utf8_lossy(&bytes[start..])
        )
    }

    #[test]
    fn a_streamed_tail_stores_exactly_what_buffering_the_whole_stream_stored() {
        // The fix is about *when* the truncation happens, not what is kept.
        // If the stored string changed, every repair round's no-progress
        // fingerprint would change with it.
        for cap in [0usize, 1, 7, 64, 1024] {
            for total in [0usize, 1, 63, 64, 65, 4096, 100_000] {
                let whole: Vec<u8> = (0..total).map(|i| b'a' + (i % 26) as u8).collect();

                // Chunk sizes a pipe realistically hands over, including ones
                // that straddle the cap in both directions.
                for chunk in [1usize, 3, 64, 997, 65_536] {
                    let mut tail = OutputTail::new(cap);
                    for piece in whole.chunks(chunk.max(1)) {
                        tail.push(piece);
                    }
                    assert_eq!(
                        tail.render(),
                        truncate_after_the_fact(&whole, cap),
                        "cap={cap} total={total} chunk={chunk}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_tail_never_holds_more_than_its_bound_however_much_is_pushed() {
        // The whole point: a check printing far more than the cap must not be
        // resident. 64 KiB cap against 8 MiB of output, in the chunk size the
        // drain actually reads.
        let cap = 64 * 1024;
        let mut tail = OutputTail::new(cap);
        let chunk = vec![b'x'; OUTPUT_READ_CHUNK_BYTES];
        let pushes = (8 * 1024 * 1024) / OUTPUT_READ_CHUNK_BYTES;
        for _ in 0..pushes {
            tail.push(&chunk);
            assert!(
                tail.buf.len() <= cap * 2 + OUTPUT_READ_CHUNK_BYTES,
                "held {} bytes against a {cap}-byte cap",
                tail.buf.len()
            );
        }
        assert_eq!(tail.total, pushes * OUTPUT_READ_CHUNK_BYTES);
        let rendered = tail.render();
        assert!(rendered.starts_with("…["), "{rendered:.40}");
        assert!(rendered.ends_with(&"x".repeat(64)));
    }
}
