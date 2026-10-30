//! Task-owned background shell workers that outlive a pause and a restart.
//!
//! A job runs under a small `/bin/sh` wrapper ([`WRAPPER`]) in its own process
//! group, with stdout/stderr appended to files in its job directory, so it does
//! not depend on Magician staying alive. The wrapper records the exit status
//! and enforces the job's deadline itself. `state.json` is the model-visible
//! receipt and never holds commands, credentials, PIDs or output;
//! `process.json` is the control record a restarted Magician re-adopts the job
//! from, and a recorded group is signalled only after an identity check. A
//! pause of the owning run keeps its jobs; its terminal state cancels them.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use super::actions::{ActionResult, BashAction};
use super::error::ExecutionError;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::subprocess_owners::{
    open_local_subprocess_owner, LocalSubprocessStore, SubprocessAccess, SubprocessOwner,
};

#[derive(Clone)]
pub(crate) struct Owner {
    pub principal: String,
    pub workspace: String,
    pub task_id: String,
    pub execution_id: String,
    pub agent_id: String,
    pub effect_id: Option<String>,
}

tokio::task_local! {
    pub(crate) static OWNER: Option<Owner>;
}

#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    job_id: String,
    task_id: String,
    execution_id: String,
    agent_id: String,
    status: String,
    started_at: String,
    ended_at: Option<String>,
    timeout_secs: Option<u64>,
    exit_code: Option<i32>,
    output_bytes: u64,
}

/// The control record a restarted Magician re-adopts a job from. Kept apart
/// from the receipt: it names a process, which the model is never handed.
#[derive(Clone, Serialize, Deserialize)]
struct ProcessRecord {
    job_id: String,
    principal: String,
    workspace: String,
    task_id: String,
    execution_id: String,
    agent_id: String,
    /// Group id, equal to the wrapper's pid (spawned with `process_group(0)`).
    pgid: u32,
    /// `System::boot_time()` at spawn; pids from an earlier boot mean nothing.
    boot_time: u64,
    /// The wrapper's start time (Unix seconds), matched against the live leader.
    spawned_at: u64,
    /// Unix seconds after which Magician kills a job the wrapper failed to stop.
    deadline_at: Option<u64>,
    /// Log bytes already dropped by the size cap.
    #[serde(default)]
    truncated_bytes: u64,
}

/// Runs a job's command, records its exit status and enforces its deadline,
/// with or without Magician. Positional arguments: `$0` names the job (the
/// identity marker), `$1` its directory, `$2` the deadline in seconds (0 =
/// none), the rest the command. The command runs in the foreground: a
/// non-interactive shell starts an `&` command with SIGINT and SIGQUIT
/// ignored, which would survive `exec` into the whole job tree, so only the
/// deadline watcher runs in the background. On exit, and at the deadline,
/// the wrapper kills its whole group (itself included), as the old in-process
/// worker did; only `$$`'s group, so it is a no-op if it is not the leader.
const WRAPPER: &str = r#"dir=$1; limit=$2; shift 2
if [ "$limit" -gt 0 ]; then
  ( /bin/sleep "$limit"; : > "$dir/timed_out"; kill -9 "-$$" ) </dev/null >/dev/null 2>&1 &
fi
"$@"
code=$?
printf '%s\n' "$code" > "$dir/exit_status.tmp" && /bin/mv -f "$dir/exit_status.tmp" "$dir/exit_status"
kill -9 "-$$"
"#;

const JOB_MARKER: &str = "magician-shell-job:";
const LOG_FILES: [&str; 2] = ["stdout.log", "stderr.log"];
/// Per log file; a longer log restarts from empty instead of filling the disk.
const LOG_CAP_BYTES: u64 = 64 * 1024 * 1024;
const TICK: Duration = Duration::from_secs(1);
/// Ticks between owner lookups while the owning run is live here. Once it
/// cancels or returns, lookups restart every tick and back off to this.
const OWNER_PROBE_TICKS: u32 = 15;
const CLOCK_SLACK_SECS: u64 = 5;
/// Consecutive "no row" probes before a job's owner counts as gone.
const MISSING_OWNER_PROBES: u32 = 3;
/// How long past its deadline Magician lets the wrapper act before killing.
const DEADLINE_GRACE_SECS: u64 = 30;

/// What the owning execution's durable row says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnerRun {
    /// Running, paused or waiting: its jobs keep running.
    Active,
    /// Completed, failed or cancelled: its jobs are cancelled.
    Terminal,
    /// The store found no row. A failed read can look the same, so this
    /// counts only once it repeats ([`MISSING_OWNER_PROBES`]).
    Missing,
    /// The store could not answer; ask again later.
    Unknown,
}

type OwnerFuture = Pin<Box<dyn Future<Output = OwnerRun> + Send>>;
pub(crate) type OwnerProbe = Arc<dyn Fn(&str) -> OwnerFuture + Send + Sync>;

static OWNER_PROBE: OnceLock<OwnerProbe> = OnceLock::new();

/// Install the durable execution lookup that tells a run's pause from its
/// terminal end. Without it (CLI, tests) the in-process run signals decide, as
/// before: cancelling or returning from the run stops its jobs.
pub(crate) fn install_owner_store(
    store: Arc<dyn crate::magician_v2::storage::V2ConversationStore>,
) {
    let probe: OwnerProbe = Arc::new(move |execution_id: &str| -> OwnerFuture {
        let store = Arc::clone(&store);
        let execution_id = execution_id.to_string();
        Box::pin(async move {
            match store.get_execution(&execution_id).await {
                Ok(execution) if execution.waiting_state.is_terminal() => OwnerRun::Terminal,
                Ok(_) => OwnerRun::Active,
                Err(crate::magician_v2::storage::V2StorageError::ExecutionNotFound(_)) => {
                    OwnerRun::Missing
                },
                Err(_) => OwnerRun::Unknown,
            }
        })
    });
    let _ = OWNER_PROBE.set(probe);
}

struct LiveJob {
    owner: Owner,
    cancel: CancellationToken,
    done: CancellationToken,
    receipt: Arc<Mutex<Receipt>>,
    logs: Logs,
}

struct Logs {
    dir: PathBuf,
    truncated: AtomicU64,
}

impl Logs {
    fn new(dir: PathBuf, truncated: u64) -> Self {
        Self {
            dir,
            truncated: AtomicU64::new(truncated),
        }
    }

    fn bytes(&self) -> u64 {
        LOG_FILES
            .iter()
            .filter_map(|name| std::fs::metadata(self.dir.join(name)).ok())
            .map(|metadata| metadata.len())
            .sum::<u64>()
            + self.truncated.load(Ordering::Relaxed)
    }

    /// Empty a log past the cap. The job holds it `O_APPEND`, so its next
    /// write lands at the new end rather than leaving a hole.
    fn cap(&self) -> bool {
        let mut capped = false;
        for name in LOG_FILES {
            let path = self.dir.join(name);
            let Ok(len) = std::fs::metadata(&path).map(|metadata| metadata.len()) else {
                continue;
            };
            if len > LOG_CAP_BYTES
                && std::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .and_then(|file| file.set_len(0))
                    .is_ok()
            {
                self.truncated.fetch_add(len, Ordering::Relaxed);
                capped = true;
            }
        }
        capped
    }
}

#[derive(Default)]
struct Registry {
    runs: HashMap<String, (CancellationToken, usize)>,
    jobs: HashMap<String, Arc<LiveJob>>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

/// Serializes launches and re-adoptions, so one job never gets two
/// supervisors. Never held across a call that takes it again.
fn launch_gate() -> &'static tokio::sync::Mutex<()> {
    static GATE: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// The outer run owns this lease: only a live run may start workers. Returning
/// or pausing no longer stops them; the owning execution's terminal state does
/// (see [`install_owner_store`]).
pub(crate) struct RunLifetime {
    execution_id: String,
    finished: CancellationToken,
}

impl RunLifetime {
    pub(crate) fn enter(execution_id: &str) -> Self {
        let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
        let (finished, frames) = registry
            .runs
            .entry(execution_id.to_string())
            .or_insert_with(|| (CancellationToken::new(), 0));
        *frames += 1;
        let finished = finished.clone();
        Self {
            execution_id: execution_id.to_string(),
            finished,
        }
    }
}

impl Drop for RunLifetime {
    fn drop(&mut self) {
        let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, frames)) = registry.runs.get_mut(&self.execution_id) {
            *frames = frames.saturating_sub(1);
            if *frames == 0 {
                self.finished.cancel();
                registry.runs.remove(&self.execution_id);
            }
        }
    }
}

pub(crate) fn running_for_execution(execution_id: &str) -> Vec<String> {
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .jobs
        .iter()
        .filter(|(_, job)| job.owner.execution_id == execution_id && !job.done.is_cancelled())
        .map(|(id, _)| id.clone())
        .collect()
}

pub(crate) fn is_background(params: &HashMap<String, Value>) -> bool {
    params
        .get("background")
        .is_some_and(|v| v.as_bool() == Some(true) || v.as_str() == Some("true"))
}

pub(crate) fn action_name(params: &HashMap<String, Value>) -> &str {
    params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("run")
}

fn owner() -> Result<Owner, ExecutionError> {
    OWNER.try_with(Clone::clone).ok().flatten().ok_or_else(|| fail(
        "Background shell jobs require a tracked task execution. Use create_task/run_task; do not detach workers from chat."
    ))
}

fn fail(message: impl Into<String>) -> ExecutionError {
    ExecutionError::AgentActionable(message.into())
}

/// One scope's job records and the owner lookup its supervisors use.
#[derive(Clone)]
struct JobStore {
    storage: LocalSubprocessStore,
    scope_root: PathBuf,
    probe: Option<OwnerProbe>,
}

impl JobStore {
    fn for_scope(layout: &ArtifactV2Workspace, principal: &str, workspace: &str) -> Self {
        Self {
            storage: open_local_subprocess_owner(
                layout,
                principal,
                workspace,
                SubprocessOwner::WorkdirsScratch,
            ),
            scope_root: layout.scope_root(principal, workspace),
            probe: OWNER_PROBE.get().cloned(),
        }
    }

    /// The directory `locator(id)` names, relative to the same scope root.
    fn dir(&self, id: &str) -> PathBuf {
        self.scope_root.join("workdirs").join("shell-jobs").join(id)
    }
}

fn store(owner: &Owner) -> JobStore {
    JobStore::for_scope(
        &crate::magician_v2::process_storage::workspace(),
        &owner.principal,
        &owner.workspace,
    )
}

fn locator(id: &str) -> Result<String, ExecutionError> {
    uuid::Uuid::parse_str(id).map_err(|_| fail("Invalid shell job_id"))?;
    Ok(format!("workdirs/shell-jobs/{id}/state.json"))
}

fn record_locator(id: &str) -> Result<String, ExecutionError> {
    uuid::Uuid::parse_str(id).map_err(|_| fail("Invalid shell job_id"))?;
    Ok(format!("workdirs/shell-jobs/{id}/process.json"))
}

async fn persist(store: &LocalSubprocessStore, receipt: &Receipt) -> Result<(), ExecutionError> {
    let bytes = serde_json::to_vec(receipt).map_err(|e| fail(e.to_string()))?;
    store
        .put(&locator(&receipt.job_id)?, &bytes)
        .await
        .map_err(|e| fail(format!("Could not persist shell job state: {e}")))
}

async fn persist_record(
    store: &LocalSubprocessStore,
    record: &ProcessRecord,
) -> Result<(), ExecutionError> {
    let bytes = serde_json::to_vec(record).map_err(|e| fail(e.to_string()))?;
    store
        .put(&record_locator(&record.job_id)?, &bytes)
        .await
        .map_err(|e| fail(format!("Could not persist shell job record: {e}")))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Launch `inner` (the already authorized, boxed `sh -c` command) under
/// [`WRAPPER`]. `inner` is read back, never spawned: its program, arguments,
/// env changes and working directory carry over unchanged, so the sandbox
/// wrapping stays intact. `build_bash_command` never calls `env_clear`, which
/// `get_envs` could not report.
#[cfg(unix)]
fn spawn_wrapper(
    inner: &tokio::process::Command,
    id: &str,
    dir: &Path,
    deadline_secs: Option<u64>,
    stdin: bool,
) -> std::io::Result<tokio::process::Child> {
    let inner = inner.as_std();
    // `/bin/sh` is absolute, so a PATH override below cannot push the spawn
    // off `posix_spawn`. The inner program is resolved against the PATH the
    // wrapper (and so the program) receives, as `build_bash_command` did.
    let mut command = tokio::process::Command::new("/bin/sh");
    let mut child_path = None;
    for (key, value) in inner.get_envs() {
        match value {
            Some(value) => {
                if key == "PATH" {
                    child_path = Some(value);
                }
                command.env(key, value);
            },
            None => {
                command.env_remove(key);
            },
        }
    }
    let program = runtime_core::process::resolve_program(inner.get_program(), child_path);
    command
        .arg("-c")
        .arg(WRAPPER)
        .arg(format!("{JOB_MARKER}{id}"))
        .arg(dir)
        .arg(deadline_secs.unwrap_or(0).to_string())
        .arg(program)
        .args(inner.get_args());
    if let Some(cwd) = inner.get_current_dir() {
        command.current_dir(cwd);
    }
    let log = |name: &str| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(name))
    };
    command
        .stdin(if stdin {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(log(LOG_FILES[0])?)
        .stderr(log(LOG_FILES[1])?)
        // Neither a Magician shutdown nor a dropped handle may end the job.
        .kill_on_drop(false)
        .process_group(0);
    command.spawn()
}

#[cfg(not(unix))]
fn spawn_wrapper(
    _inner: &tokio::process::Command,
    _id: &str,
    _dir: &Path,
    _deadline_secs: Option<u64>,
    _stdin: bool,
) -> std::io::Result<tokio::process::Child> {
    Err(std::io::Error::other(
        "background shell jobs require a Unix host",
    ))
}

/// What the wrapper left behind: its exit status, and whether its deadline
/// fired. Read only once the wrapper is gone.
fn recorded_outcome(dir: &Path) -> (Option<i32>, bool) {
    let code = std::fs::read_to_string(dir.join("exit_status"))
        .ok()
        .and_then(|text| text.trim().parse().ok());
    (code, dir.join("timed_out").exists())
}

#[cfg(unix)]
mod host {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, System, UpdateKind};

    pub(super) struct Leader {
        pub start_time: u64,
        pub cmd: Vec<String>,
    }

    pub(super) fn boot_time() -> u64 {
        System::boot_time()
    }

    /// The live process `pid`, if any; a zombie has already exited.
    pub(super) fn leader(pid: u32) -> Option<Leader> {
        let pid = Pid::from_u32(pid);
        let mut system = System::new();
        if !system
            .refresh_process_specifics(pid, ProcessRefreshKind::new().with_cmd(UpdateKind::Always))
        {
            return None;
        }
        let process = system.process(pid)?;
        if matches!(
            process.status(),
            ProcessStatus::Zombie | ProcessStatus::Dead
        ) {
            return None;
        }
        Some(Leader {
            start_time: process.start_time(),
            cmd: process.cmd().to_vec(),
        })
    }

    /// Whether a process group we may signal still has members.
    pub(super) fn group_alive(pgid: u32) -> bool {
        let Ok(pgid) = i32::try_from(pgid) else {
            return false;
        };
        if pgid <= 1 {
            return false;
        }
        // SAFETY: signal 0 only probes for existence and permission.
        unsafe { libc::kill(-pgid, 0) == 0 }
    }

    pub(super) fn kill_group(pgid: u32) {
        let Ok(pgid) = i32::try_from(pgid) else {
            return;
        };
        // `kill(0)` would signal our own group and `kill(-1)` every process we
        // may signal; a recorded id read back from disk must never become either.
        // SAFETY: `getpgrp` has no preconditions and cannot fail.
        if pgid <= 1 || pgid == unsafe { libc::getpgrp() } {
            return;
        }
        // SAFETY: `kill(2)` with a negative pid signals a process group and
        // touches no Rust memory; a group that no longer exists yields ESRCH.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Identity {
    /// The recorded wrapper is still running.
    Alive,
    /// The wrapper is gone but members of its group live on. The group id
    /// stays reserved while any member lives, so it is still ours.
    Leaderless,
    /// Nothing of the job is left.
    Gone,
    /// The id now names something else (a reused pid, another boot, our own
    /// group): never signal it.
    Foreign,
}

/// Whether a recorded group is still this job: same boot, not our own
/// process or group, and a live leader that started when the record says and
/// carries the job's marker. The `magician-learning` bot reaper's rule, plus
/// the marker.
#[cfg(unix)]
fn identity(record: &ProcessRecord) -> Identity {
    if record.pgid <= 1 || record.pgid == std::process::id() {
        return Identity::Foreign;
    }
    // SAFETY: `getpgrp` has no preconditions and cannot fail.
    let own_group = unsafe { libc::getpgrp() };
    if i32::try_from(record.pgid).is_ok_and(|pgid| pgid == own_group) {
        return Identity::Foreign;
    }
    if host::boot_time().abs_diff(record.boot_time) > CLOCK_SLACK_SECS {
        return Identity::Foreign;
    }
    match host::leader(record.pgid) {
        Some(leader) => {
            let marker = format!("{JOB_MARKER}{}", record.job_id);
            let started = leader.start_time.abs_diff(record.spawned_at) <= CLOCK_SLACK_SECS;
            // An unreadable argv leaves the start-time check alone.
            let named = leader.cmd.is_empty() || leader.cmd.iter().any(|arg| *arg == marker);
            if started && named {
                Identity::Alive
            } else {
                Identity::Foreign
            }
        },
        None if host::group_alive(record.pgid) => Identity::Leaderless,
        None => Identity::Gone,
    }
}

#[cfg(not(unix))]
fn identity(_record: &ProcessRecord) -> Identity {
    Identity::Foreign
}

/// Kill a recorded group, only after it proves to be this job.
fn kill_if_ours(record: &ProcessRecord) {
    #[cfg(unix)]
    if matches!(identity(record), Identity::Alive | Identity::Leaderless) {
        host::kill_group(record.pgid);
    }
    #[cfg(not(unix))]
    let _ = record;
}

pub(crate) async fn start(
    action: &BashAction,
    command: tokio::process::Command,
    timeout_secs: Option<u64>,
) -> Result<ActionResult, ExecutionError> {
    let owner = owner()?;
    let parent = super::compiled_dispatch::EXECUTION_CANCEL_TOKEN
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .ok_or_else(|| fail("Background shell jobs require the owning run's cancellation token"))?;
    let finished = registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .runs
        .get(&owner.execution_id)
        .map(|(token, _)| token.clone())
        .ok_or_else(|| fail("The owning task is no longer running"))?;
    if parent.is_cancelled() || finished.is_cancelled() {
        return Err(fail("The owning run has stopped"));
    }
    let store = store(&owner);
    start_stored(
        action,
        &command,
        timeout_secs,
        Launch {
            owner,
            parent,
            finished,
            store,
        },
    )
    .await
}

struct Launch {
    owner: Owner,
    parent: CancellationToken,
    finished: CancellationToken,
    store: JobStore,
}

async fn start_stored(
    action: &BashAction,
    command: &tokio::process::Command,
    timeout_secs: Option<u64>,
    launch: Launch,
) -> Result<ActionResult, ExecutionError> {
    let Launch {
        owner,
        parent,
        finished,
        store,
    } = launch;
    let launch_guard = launch_gate().lock().await;
    if parent.is_cancelled() || finished.is_cancelled() {
        return Err(fail("The owning run has stopped"));
    }
    // One model-call identity owns one launch, even if its result delivery retries.
    let id = if let Some(effect) = owner.effect_id.as_deref() {
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        for value in [
            owner.principal.as_str(),
            owner.workspace.as_str(),
            owner.task_id.as_str(),
            owner.execution_id.as_str(),
            owner.agent_id.as_str(),
            effect,
        ] {
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value.as_bytes());
        }
        let digest = digest.finalize();
        let mut bytes = [0; 16];
        bytes.copy_from_slice(&digest[..16]);
        uuid::Uuid::from_bytes(bytes).to_string()
    } else {
        uuid::Uuid::new_v4().to_string()
    };
    if store
        .storage
        .exists(&locator(&id)?)
        .await
        .map_err(|e| fail(e.to_string()))?
    {
        // `inspect_stored` may re-adopt, which takes the gate.
        drop(launch_guard);
        return inspect_stored(
            &HashMap::from([("job_id".to_string(), Value::String(id))]),
            false,
            owner,
            store,
        )
        .await;
    }
    if running_for_execution(&owner.execution_id).len() >= 16 {
        return Err(fail(
            "This run already owns 16 shell jobs; wait for or cancel an existing job first",
        ));
    }
    let mut receipt = Receipt {
        job_id: id.clone(),
        task_id: owner.task_id.clone(),
        execution_id: owner.execution_id.clone(),
        agent_id: owner.agent_id.clone(),
        status: "running".to_string(),
        started_at: chrono::Utc::now().to_rfc3339(),
        ended_at: None,
        timeout_secs,
        exit_code: None,
        output_bytes: 0,
    };
    // Persistence must succeed BEFORE side effects start.
    persist(&store.storage, &receipt).await?;
    if parent.is_cancelled() || finished.is_cancelled() {
        receipt.status = if parent.is_cancelled() {
            "cancelled"
        } else {
            "interrupted"
        }
        .to_string();
        receipt.ended_at = Some(chrono::Utc::now().to_rfc3339());
        persist(&store.storage, &receipt).await?;
        return render(&receipt);
    }
    let dir = store.dir(&id);
    let spawn = std::fs::create_dir_all(&dir)
        .and_then(|()| spawn_wrapper(command, &id, &dir, timeout_secs, action.stdin.is_some()));
    let mut child = match spawn {
        Ok(child) => child,
        Err(error) => {
            let mut failed = receipt;
            failed.status = "failed_to_start".to_string();
            failed.ended_at = Some(chrono::Utc::now().to_rfc3339());
            persist(&store.storage, &failed).await?;
            return Err(fail(format!("Shell job {id} failed to start: {error}")));
        },
    };
    let pgid = child.id().unwrap_or(0);
    #[cfg(unix)]
    let spawned_at = host::leader(pgid).map_or_else(unix_now, |leader| leader.start_time);
    #[cfg(not(unix))]
    let spawned_at = unix_now();
    let record = ProcessRecord {
        job_id: id.clone(),
        principal: owner.principal.clone(),
        workspace: owner.workspace.clone(),
        task_id: owner.task_id.clone(),
        execution_id: owner.execution_id.clone(),
        agent_id: owner.agent_id.clone(),
        pgid,
        #[cfg(unix)]
        boot_time: host::boot_time(),
        #[cfg(not(unix))]
        boot_time: 0,
        spawned_at,
        deadline_at: timeout_secs.map(|seconds| unix_now().saturating_add(seconds)),
        truncated_bytes: 0,
    };
    // Without the record the job still runs (and the wrapper still bounds
    // it), but a restart reports it interrupted rather than re-adopting it.
    if let Err(error) = persist_record(&store.storage, &record).await {
        tracing::warn!(job_id = %id, %error, "shell job control record write failed");
    }
    if let (Some(payload), Some(mut stdin)) = (action.stdin.clone(), child.stdin.take()) {
        tokio::spawn(async move {
            let _ = stdin.write_all(payload.as_bytes()).await;
        });
    }
    let job = Arc::new(LiveJob {
        owner,
        cancel: CancellationToken::new(),
        done: CancellationToken::new(),
        receipt: Arc::new(Mutex::new(receipt.clone())),
        logs: Logs::new(dir, 0),
    });
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .jobs
        .insert(id.clone(), job.clone());
    // Only frozen command/process handles cross this spawn. Authorization,
    // grants, CWD and sandbox resolution happened in the calling task.
    tokio::spawn(supervise(
        Supervised {
            id,
            job,
            record,
            store,
            local: Some((parent, finished)),
        },
        Process::Child(child),
    ));
    drop(launch_guard);
    render(&receipt)
}

enum Process {
    /// Spawned by this Magician process, which reaps it.
    Child(tokio::process::Child),
    /// Re-adopted from its record after a restart; watched by identity.
    Adopted,
}

#[derive(Clone, Copy)]
enum End {
    /// The wrapper this process spawned exited.
    Exited,
    /// An adopted wrapper is gone, or its id names something else.
    Lost,
    Cancelled,
    /// The owning execution reached a terminal state.
    OwnerEnded,
    TimedOut,
    /// No owner lookup is installed: the run's in-process signal decided.
    Legacy(&'static str),
}

struct Supervised {
    id: String,
    job: Arc<LiveJob>,
    record: ProcessRecord,
    store: JobStore,
    /// The owning run's cancellation and return signals, for a job launched
    /// by this process.
    local: Option<(CancellationToken, CancellationToken)>,
}

async fn supervise(supervised: Supervised, mut process: Process) {
    let Supervised {
        id,
        job,
        mut record,
        store,
        local,
    } = supervised;
    let mut watch_local = local.is_some();
    // An adopted job's owner is looked up on the first tick.
    let mut probe_in: u32 = if local.is_some() {
        OWNER_PROBE_TICKS
    } else {
        0
    };
    let mut probe_every = OWNER_PROBE_TICKS;
    let mut missing_probes: u32 = 0;
    let end = loop {
        let exited = async {
            match &mut process {
                Process::Child(child) => {
                    let _ = child.wait().await;
                },
                Process::Adopted => std::future::pending::<()>().await,
            }
        };
        let watching = watch_local;
        let owner_signal = async {
            match &local {
                Some((parent, finished)) if watching => {
                    tokio::select! {
                        _ = parent.cancelled() => {},
                        _ = finished.cancelled() => {},
                    }
                },
                _ => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            () = exited => break End::Exited,
            () = job.cancel.cancelled() => break End::Cancelled,
            () = owner_signal => {
                if store.probe.is_none() {
                    let cancelled = local.as_ref().is_some_and(|(parent, _)| parent.is_cancelled());
                    break End::Legacy(if cancelled { "cancelled" } else { "interrupted" });
                }
                // A pause and a terminal end both start here; the durable row
                // tells them apart once the run settles.
                watch_local = false;
                probe_in = 0;
                probe_every = 1;
                continue;
            },
            () = tokio::time::sleep(TICK) => {},
        }
        if matches!(process, Process::Adopted) && identity(&record) != Identity::Alive {
            break End::Lost;
        }
        if job.logs.cap() {
            record.truncated_bytes = job.logs.truncated.load(Ordering::Relaxed);
            let _ = persist_record(&store.storage, &record).await;
        }
        if record
            .deadline_at
            .is_some_and(|at| unix_now() > at.saturating_add(DEADLINE_GRACE_SECS))
        {
            break End::TimedOut;
        }
        if let Some(probe) = &store.probe {
            if probe_in == 0 {
                match probe(&record.execution_id).await {
                    OwnerRun::Terminal => break End::OwnerEnded,
                    OwnerRun::Missing => {
                        missing_probes += 1;
                        if missing_probes >= MISSING_OWNER_PROBES {
                            break End::OwnerEnded;
                        }
                    },
                    OwnerRun::Active | OwnerRun::Unknown => missing_probes = 0,
                }
                probe_in = probe_every;
                probe_every = (probe_every * 2).min(OWNER_PROBE_TICKS);
            }
            probe_in -= 1;
        }
    };
    let stop = matches!(
        end,
        End::Cancelled | End::OwnerEnded | End::TimedOut | End::Legacy(_)
    );
    match &mut process {
        Process::Child(child) => {
            // Signal before reaping: the unreaped leader keeps the group id ours.
            #[cfg(unix)]
            if stop {
                host::kill_group(record.pgid);
            }
            let _ = child.wait().await;
        },
        Process::Adopted => {
            if stop {
                kill_if_ours(&record);
            }
        },
    }
    let mut receipt = job
        .receipt
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let settled = settle(&mut receipt, &job.logs, &record, &store, end, true).await;
    *job.receipt.lock().unwrap_or_else(|e| e.into_inner()) = receipt;
    // Keep the live terminal receipt visible if persistence fails; never
    // expose an old durable "running" record as a successful completion.
    if let Err(error) = settled {
        tracing::error!(job_id = %id, %error, "shell job terminal state write failed");
    } else {
        registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .jobs
            .remove(&id);
    }
    job.done.cancel();
}

/// Record a job's terminal receipt from what its wrapper left behind and why
/// supervision ended, then retire its control record.
async fn settle(
    receipt: &mut Receipt,
    logs: &Logs,
    record: &ProcessRecord,
    store: &JobStore,
    end: End,
    leader_seen: bool,
) -> Result<(), ExecutionError> {
    let dir = store.dir(&record.job_id);
    let (code, timed_out) = recorded_outcome(&dir);
    // A wrapper killed before its epilogue (from inside the job, say) can
    // leave the rest of its group running. Only a supervisor that saw the
    // wrapper alive may signal it: at adoption a leaderless group id may
    // already belong to someone else's processes (pid reuse).
    if leader_seen && code.is_none() && identity(record) == Identity::Leaderless {
        kill_if_ours(record);
    }
    receipt.status = match (code, end) {
        (Some(0), _) => "completed",
        (Some(_), _) => "failed",
        (None, End::Legacy(status)) => status,
        (None, End::Cancelled | End::OwnerEnded) => "cancelled",
        (None, End::TimedOut) => "timed_out",
        (None, End::Exited | End::Lost) if timed_out => "timed_out",
        (None, End::Exited) => "failed",
        (None, End::Lost) => "interrupted",
    }
    .to_string();
    receipt.exit_code = code;
    receipt.output_bytes = logs.bytes();
    receipt.ended_at = Some(chrono::Utc::now().to_rfc3339());
    persist(&store.storage, receipt).await?;
    let _ = std::fs::remove_file(dir.join("process.json"));
    Ok(())
}

/// Re-adopt a job a previous Magician process recorded as running: watch it
/// again if its wrapper is still the recorded one, otherwise settle it from
/// what the wrapper left behind. A no-op for a job already live here.
async fn adopt(id: &str, store: &JobStore) -> Result<(), ExecutionError> {
    let _launch = launch_gate().lock().await;
    if registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .jobs
        .contains_key(id)
    {
        return Ok(());
    }
    let Ok(bytes) = store.storage.get(&locator(id)?).await else {
        return Ok(());
    };
    let mut receipt: Receipt =
        serde_json::from_slice(&bytes).map_err(|_| fail("Invalid shell job receipt"))?;
    if receipt.status != "running" {
        return Ok(());
    }
    let record = store
        .storage
        .get(&record_locator(id)?)
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ProcessRecord>(&bytes).ok())
        .filter(|record| {
            record.job_id == receipt.job_id
                && record.task_id == receipt.task_id
                && record.execution_id == receipt.execution_id
                && record.agent_id == receipt.agent_id
        });
    let Some(record) = record else {
        // No control record (a job from before restart survival, or a crash
        // between spawn and record): there is nothing safe to signal.
        receipt.status = "interrupted".to_string();
        receipt.ended_at = Some(chrono::Utc::now().to_rfc3339());
        return persist(&store.storage, &receipt).await;
    };
    let logs = Logs::new(store.dir(id), record.truncated_bytes);
    if identity(&record) != Identity::Alive {
        return settle(&mut receipt, &logs, &record, store, End::Lost, false).await;
    }
    let job = Arc::new(LiveJob {
        owner: Owner {
            principal: record.principal.clone(),
            workspace: record.workspace.clone(),
            task_id: record.task_id.clone(),
            execution_id: record.execution_id.clone(),
            agent_id: record.agent_id.clone(),
            effect_id: None,
        },
        cancel: CancellationToken::new(),
        done: CancellationToken::new(),
        receipt: Arc::new(Mutex::new(receipt)),
        logs,
    });
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .jobs
        .insert(id.to_string(), job.clone());
    tokio::spawn(supervise(
        Supervised {
            id: id.to_string(),
            job,
            record,
            store: store.clone(),
            local: None,
        },
        Process::Adopted,
    ));
    Ok(())
}

/// Boot sweep: re-adopt every job a previous Magician process left running,
/// in every scope under `layout`. A job whose owner already ended is
/// cancelled on its first supervision tick.
pub(crate) async fn adopt_recorded(layout: ArtifactV2Workspace) {
    let Ok(principals) = std::fs::read_dir(layout.scopes_root()) else {
        return;
    };
    for principal in principals.flatten() {
        let Ok(workspaces) = std::fs::read_dir(principal.path()) else {
            continue;
        };
        for workspace in workspaces.flatten() {
            let jobs = workspace.path().join("workdirs").join("shell-jobs");
            let Ok(entries) = std::fs::read_dir(&jobs) else {
                continue;
            };
            for entry in entries.flatten() {
                let Some(record) = std::fs::read(entry.path().join("process.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<ProcessRecord>(&bytes).ok())
                else {
                    continue;
                };
                let store = JobStore::for_scope(&layout, &record.principal, &record.workspace);
                // A record names its own scope and job; a copy elsewhere adopts nothing.
                if store.scope_root != workspace.path()
                    || entry.file_name().to_str() != Some(record.job_id.as_str())
                {
                    continue;
                }
                if let Err(error) = adopt(&record.job_id, &store).await {
                    tracing::warn!(job_id = %record.job_id, %error, "shell job re-adoption failed");
                }
            }
        }
    }
}

fn render(receipt: &Receipt) -> Result<ActionResult, ExecutionError> {
    let value = serde_json::to_string(receipt).map_err(|e| fail(e.to_string()))?;
    Ok(ActionResult::text(value))
}

pub(crate) async fn inspect(
    params: &HashMap<String, Value>,
    cancel: bool,
) -> Result<ActionResult, ExecutionError> {
    let owner = owner()?;
    let store = store(&owner);
    inspect_stored(params, cancel, owner, store).await
}

async fn inspect_stored(
    params: &HashMap<String, Value>,
    cancel: bool,
    owner: Owner,
    store: JobStore,
) -> Result<ActionResult, ExecutionError> {
    let id = params
        .get("job_id")
        .and_then(Value::as_str)
        .ok_or_else(|| fail("shell status/cancel requires job_id"))?;
    let path = locator(id)?;
    let live = || {
        registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .jobs
            .get(id)
            .cloned()
    };
    let mut job = live();
    if job.is_none() {
        // Not launched by this process: a recorded running job is re-adopted
        // (or settled) before it is reported.
        let bytes = storage_get(&store, &path).await?;
        let receipt: Receipt =
            serde_json::from_slice(&bytes).map_err(|_| fail("Invalid shell job receipt"))?;
        if receipt.task_id != owner.task_id || receipt.agent_id != owner.agent_id {
            return Err(fail("Shell job belongs to a different task or agent"));
        }
        if receipt.status == "running" {
            adopt(id, &store).await?;
            job = live();
        }
    }
    if let Some(job) = job {
        // A resumed or successor execution of the same task and agent may
        // inspect the jobs its run started, as the durable path always allowed.
        if job.owner.principal != owner.principal
            || job.owner.workspace != owner.workspace
            || job.owner.task_id != owner.task_id
            || job.owner.agent_id != owner.agent_id
        {
            return Err(fail("Shell job belongs to a different task execution"));
        }
        if cancel {
            job.cancel.cancel();
        }
        let wait = params
            .get("wait_secs")
            .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
            .unwrap_or(0)
            .min(30);
        if wait > 0 {
            let _ = tokio::time::timeout(Duration::from_secs(wait), job.done.cancelled()).await;
        }
        let mut receipt = job
            .receipt
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if receipt.status == "running" {
            receipt.output_bytes = job.logs.bytes();
        }
        return render(&receipt);
    }
    let bytes = storage_get(&store, &path).await?;
    let receipt: Receipt =
        serde_json::from_slice(&bytes).map_err(|_| fail("Invalid shell job receipt"))?;
    if receipt.task_id != owner.task_id || receipt.agent_id != owner.agent_id {
        return Err(fail("Shell job belongs to a different task or agent"));
    }
    render(&receipt)
}

async fn storage_get(store: &JobStore, path: &str) -> Result<Vec<u8>, ExecutionError> {
    store
        .storage
        .get(path)
        .await
        .map_err(|_| fail("Shell job was not found in this scope"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::shell_process::ProcessGroup;

    fn test_owner(effect_id: Option<&str>) -> Owner {
        Owner {
            principal: "test-owner".to_string(),
            workspace: "default".to_string(),
            task_id: uuid::Uuid::new_v4().to_string(),
            execution_id: uuid::Uuid::new_v4().to_string(),
            agent_id: "worker".to_string(),
            effect_id: effect_id.map(str::to_string),
        }
    }

    fn test_store(root: &Path, owner: &Owner, probe: Option<OwnerProbe>) -> JobStore {
        let layout = ArtifactV2Workspace::new(root);
        JobStore {
            storage: open_local_subprocess_owner(
                &layout,
                owner.principal.as_str(),
                owner.workspace.as_str(),
                SubprocessOwner::WorkdirsScratch,
            ),
            scope_root: layout.scope_root(&owner.principal, &owner.workspace),
            probe,
        }
    }

    /// An owner lookup the test flips between a pause and a terminal end.
    fn switch_probe(state: Arc<Mutex<OwnerRun>>) -> OwnerProbe {
        Arc::new(move |_execution_id: &str| -> OwnerFuture {
            let run = *state.lock().unwrap();
            Box::pin(async move { run })
        })
    }

    fn shell(command_text: &str, cwd: &Path) -> tokio::process::Command {
        let mut command = tokio::process::Command::new("/bin/sh");
        command.arg("-c").arg(command_text).current_dir(cwd);
        ProcessGroup::configure(&mut command);
        command
    }

    fn receipt_of(result: ActionResult) -> Receipt {
        serde_json::from_str(result.as_text().unwrap()).unwrap()
    }

    fn job_params(id: &str, wait_secs: u64) -> HashMap<String, Value> {
        HashMap::from([
            ("job_id".to_string(), Value::String(id.to_string())),
            ("wait_secs".to_string(), Value::from(wait_secs)),
        ])
    }

    fn running_receipt(id: &str, owner: &Owner) -> Receipt {
        Receipt {
            job_id: id.to_string(),
            task_id: owner.task_id.clone(),
            execution_id: owner.execution_id.clone(),
            agent_id: owner.agent_id.clone(),
            status: "running".to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
            ended_at: None,
            timeout_secs: Some(60),
            exit_code: None,
            output_bytes: 0,
        }
    }

    #[cfg(unix)]
    fn record_for(id: &str, owner: &Owner, pgid: u32, spawned_at: u64) -> ProcessRecord {
        ProcessRecord {
            job_id: id.to_string(),
            principal: owner.principal.clone(),
            workspace: owner.workspace.clone(),
            task_id: owner.task_id.clone(),
            execution_id: owner.execution_id.clone(),
            agent_id: owner.agent_id.clone(),
            pgid,
            boot_time: host::boot_time(),
            spawned_at,
            deadline_at: Some(unix_now() + 60),
            truncated_bytes: 0,
        }
    }

    /// Killed members are reaped by their new parent asynchronously.
    #[cfg(unix)]
    async fn group_gone(pgid: u32) -> bool {
        for _ in 0..50 {
            if !host::group_alive(pgid) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    #[test]
    fn job_ids_cannot_be_filesystem_paths() {
        for id in ["../state", "/tmp/pid", "123", ""] {
            assert!(locator(id).is_err());
            assert!(record_locator(id).is_err());
        }
        assert!(locator(&uuid::Uuid::new_v4().to_string()).is_ok());
    }

    #[tokio::test]
    async fn model_arguments_cannot_create_an_owner_or_control_a_pid() {
        let params = HashMap::from([
            ("__task_id".to_string(), Value::String("forged".to_string())),
            ("job_id".to_string(), Value::String("123".to_string())),
        ]);
        assert!(inspect(&params, true)
            .await
            .unwrap_err()
            .to_string()
            .contains("tracked task"));
    }

    #[test]
    fn returning_from_a_run_retires_its_worker_lifetime() {
        let id = uuid::Uuid::new_v4().to_string();
        let lifetime = RunLifetime::enter(&id);
        let signal = registry().lock().unwrap().runs[&id].0.clone();
        let nested = RunLifetime::enter(&id);
        drop(nested);
        assert!(!signal.is_cancelled());
        drop(lifetime);
        assert!(signal.is_cancelled());
        assert!(!registry().lock().unwrap().runs.contains_key(&id));
    }

    #[cfg(unix)]
    async fn worker_fixture(
        command_text: &str,
        deadline: Option<u64>,
        cancel: bool,
    ) -> (Receipt, Vec<u8>) {
        let temp = tempfile::TempDir::new().unwrap();
        let owner = test_owner(Some("one-call"));
        // No owner lookup: the run's in-process signals decide.
        let store = test_store(temp.path(), &owner, None);
        let parent = CancellationToken::new();
        let finished = CancellationToken::new();
        let action = BashAction::new(command_text);
        let result = start_stored(
            &action,
            &shell(command_text, temp.path()),
            deadline,
            Launch {
                owner: owner.clone(),
                parent: parent.clone(),
                finished: finished.clone(),
                store: store.clone(),
            },
        )
        .await
        .unwrap();
        let receipt = receipt_of(result);
        // A retry of the SAME admitted call returns its handle, never another worker.
        let repeated = start_stored(
            &action,
            &shell(command_text, temp.path()),
            deadline,
            Launch {
                owner: owner.clone(),
                parent: parent.clone(),
                finished,
                store: store.clone(),
            },
        )
        .await
        .unwrap();
        assert_eq!(receipt.job_id, receipt_of(repeated).job_id);
        let mut params = job_params(&receipt.job_id, 3);
        let mut stranger = owner.clone();
        stranger.task_id = "another-task".to_string();
        assert!(inspect_stored(&params, true, stranger, store.clone())
            .await
            .is_err());
        if cancel {
            parent.cancel();
        }
        let result = inspect_stored(&params, false, owner.clone(), store.clone())
            .await
            .unwrap();
        let final_receipt = receipt_of(result);
        assert_ne!(final_receipt.status, "running");
        assert!(running_for_execution(&owner.execution_id).is_empty());
        // Terminal reads come from durable state, with no raw output/command/PID.
        params.remove("wait_secs");
        let persisted = store
            .storage
            .get(&locator(&receipt.job_id).unwrap())
            .await
            .unwrap();
        // A settled job leaves no control record behind.
        assert!(!store.dir(&receipt.job_id).join("process.json").exists());
        let result = inspect_stored(&params, false, owner, store).await.unwrap();
        assert_eq!(receipt_of(result).status, final_receipt.status);
        (final_receipt, persisted)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn completed_worker_has_a_durable_result_without_persisting_its_input() {
        let (receipt, bytes) = worker_fixture("printf 'sensitive-output'", Some(10), false).await;
        assert_eq!(receipt.status, "completed");
        assert_eq!(receipt.exit_code, Some(0));
        assert_eq!(receipt.output_bytes, "sensitive-output".len() as u64);
        let text = String::from_utf8(bytes).unwrap();
        for forbidden in ["sensitive-output", "command", "stdin", "pid", "env"] {
            assert!(!text.contains(forbidden));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failing_worker_reports_the_exit_status_its_wrapper_recorded() {
        let (receipt, _) = worker_fixture("exit 3", Some(10), false).await;
        assert_eq!(receipt.status, "failed");
        assert_eq!(receipt.exit_code, Some(3));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn parent_cancellation_stops_and_retires_a_worker_without_an_owner_lookup() {
        let (receipt, _) = worker_fixture("sleep 30", None, true).await;
        assert_eq!(receipt.status, "cancelled");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn explicit_deadline_stops_a_worker_and_its_background_children() {
        let (receipt, _) = worker_fixture("sleep 30 & wait", Some(1), false).await;
        assert_eq!(receipt.status, "timed_out");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_pause_keeps_the_job_and_a_terminal_end_cancels_it() {
        let temp = tempfile::TempDir::new().unwrap();
        let owner = test_owner(None);
        let run = Arc::new(Mutex::new(OwnerRun::Active));
        let store = test_store(temp.path(), &owner, Some(switch_probe(run.clone())));
        let parent = CancellationToken::new();
        let finished = CancellationToken::new();
        let started = receipt_of(
            start_stored(
                &BashAction::new("sleep 30"),
                &shell("sleep 30", temp.path()),
                None,
                Launch {
                    owner: owner.clone(),
                    parent: parent.clone(),
                    finished: finished.clone(),
                    store: store.clone(),
                },
            )
            .await
            .unwrap(),
        );
        let record: ProcessRecord = serde_json::from_slice(
            &store
                .storage
                .get(&record_locator(&started.job_id).unwrap())
                .await
                .unwrap(),
        )
        .unwrap();
        // A pause cancels the run's token and returns from the run; its row
        // stays non-terminal (Paused).
        parent.cancel();
        finished.cancel();
        let paused = receipt_of(
            inspect_stored(
                &job_params(&started.job_id, 3),
                false,
                owner.clone(),
                store.clone(),
            )
            .await
            .unwrap(),
        );
        assert_eq!(paused.status, "running");
        assert!(host::group_alive(record.pgid));
        assert_eq!(
            running_for_execution(&owner.execution_id),
            vec![started.job_id.clone()]
        );
        // The run ends: its jobs are cancelled.
        *run.lock().unwrap() = OwnerRun::Terminal;
        let ended = receipt_of(
            inspect_stored(
                &job_params(&started.job_id, 20),
                false,
                owner.clone(),
                store,
            )
            .await
            .unwrap(),
        );
        assert_eq!(ended.status, "cancelled");
        assert!(running_for_execution(&owner.execution_id).is_empty());
        assert!(group_gone(record.pgid).await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_recorded_live_job_is_readopted_after_a_restart() {
        let temp = tempfile::TempDir::new().unwrap();
        let owner = test_owner(None);
        let run = Arc::new(Mutex::new(OwnerRun::Active));
        let store = test_store(temp.path(), &owner, Some(switch_probe(run)));
        let id = uuid::Uuid::new_v4().to_string();
        let dir = store.dir(&id);
        std::fs::create_dir_all(&dir).unwrap();
        // The previous Magician process launched this job and is gone: here
        // only a reaper waits on the wrapper; supervision must be re-adopted.
        let mut wrapper =
            spawn_wrapper(&shell("sleep 30", temp.path()), &id, &dir, Some(60), false).unwrap();
        let pgid = wrapper.id().unwrap();
        let reaper = tokio::spawn(async move { wrapper.wait().await });
        let spawned_at = host::leader(pgid).unwrap().start_time;
        persist(&store.storage, &running_receipt(&id, &owner))
            .await
            .unwrap();
        persist_record(&store.storage, &record_for(&id, &owner, pgid, spawned_at))
            .await
            .unwrap();
        let status = receipt_of(
            inspect_stored(&job_params(&id, 0), false, owner.clone(), store.clone())
                .await
                .unwrap(),
        );
        assert_eq!(status.status, "running");
        assert_eq!(running_for_execution(&owner.execution_id), vec![id.clone()]);
        let cancelled = receipt_of(
            inspect_stored(&job_params(&id, 10), true, owner.clone(), store)
                .await
                .unwrap(),
        );
        assert_eq!(cancelled.status, "cancelled");
        let _ = reaper.await;
        assert!(group_gone(pgid).await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_recycled_pid_record_is_never_signalled() {
        let temp = tempfile::TempDir::new().unwrap();
        let owner = test_owner(None);
        let store = test_store(temp.path(), &owner, None);
        // An unrelated process now holds the recorded id.
        let mut unrelated = tokio::process::Command::new("/bin/sleep");
        unrelated.arg("30").process_group(0).kill_on_drop(true);
        let mut unrelated = unrelated.spawn().unwrap();
        let pgid = unrelated.id().unwrap();
        let started = host::leader(pgid).unwrap().start_time;
        // A start time from another process, and the right start time without
        // the job's marker: neither is ours.
        for spawned_at in [started.saturating_sub(3600), started] {
            let id = uuid::Uuid::new_v4().to_string();
            persist(&store.storage, &running_receipt(&id, &owner))
                .await
                .unwrap();
            persist_record(&store.storage, &record_for(&id, &owner, pgid, spawned_at))
                .await
                .unwrap();
            let result = receipt_of(
                inspect_stored(&job_params(&id, 0), true, owner.clone(), store.clone())
                    .await
                    .unwrap(),
            );
            assert_eq!(result.status, "interrupted");
            assert_eq!(result.exit_code, None);
            assert!(host::group_alive(pgid));
        }
        let _ = unrelated.kill().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_wrapper_records_the_exit_status_and_output() {
        let temp = tempfile::TempDir::new().unwrap();
        let dir = temp.path().join("job");
        std::fs::create_dir_all(&dir).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let mut wrapper = spawn_wrapper(
            &shell("read line; echo \"got $line\"; exit 3", temp.path()),
            &id,
            &dir,
            None,
            true,
        )
        .unwrap();
        let mut stdin = wrapper.stdin.take().unwrap();
        stdin.write_all(b"input\n").await.unwrap();
        drop(stdin);
        let _ = wrapper.wait().await;
        assert_eq!(recorded_outcome(&dir), (Some(3), false));
        assert_eq!(
            std::fs::read_to_string(dir.join("stdout.log")).unwrap(),
            "got input\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_wrapper_enforces_the_deadline_on_its_own() {
        let temp = tempfile::TempDir::new().unwrap();
        let dir = temp.path().join("job");
        std::fs::create_dir_all(&dir).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let mut wrapper = spawn_wrapper(
            &shell("sleep 30 & sleep 30", temp.path()),
            &id,
            &dir,
            Some(1),
            false,
        )
        .unwrap();
        let pgid = wrapper.id().unwrap();
        // Nothing but the wrapper stops it.
        assert!(
            tokio::time::timeout(Duration::from_secs(10), wrapper.wait())
                .await
                .is_ok()
        );
        assert_eq!(recorded_outcome(&dir), (None, true));
        assert!(group_gone(pgid).await);
    }

    #[tokio::test]
    async fn restart_marks_a_job_without_a_control_record_interrupted() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut owner = test_owner(None);
        let store = test_store(temp.path(), &owner, None);
        let mut receipt = running_receipt(&uuid::Uuid::new_v4().to_string(), &owner);
        receipt.execution_id = "exec-old".to_string();
        owner.execution_id = "exec-new".to_string();
        persist(&store.storage, &receipt).await.unwrap();
        let params = HashMap::from([("job_id".to_string(), Value::String(receipt.job_id))]);
        let result = receipt_of(inspect_stored(&params, true, owner, store).await.unwrap());
        assert_eq!(result.status, "interrupted");
        assert_eq!(result.exit_code, None);
    }
}
