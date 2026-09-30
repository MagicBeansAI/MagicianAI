//! Process-tree owner for governed CLI authentication lifecycles.
//!
//! The executor accepts only a sealed `tool-runtime-core` invocation. It never invokes a
//! shell, clears the child environment before applying the exact bound values, drains
//! output through fixed caps, owns the full process group/session, and reaps it on every
//! normal, cancellation, timeout, output-limit, bridge-error, and unwind path. No catalog
//! The Google Workspace family is the first production consumer; other
//! providers remain gated until their own migrations.

#![allow(
    dead_code,
    reason = "interactive lifecycle variants remain reserved for later provider migrations"
)]

use std::{
    error::Error,
    fmt,
    io::{Read, Write},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::fd::{AsRawFd, RawFd};
#[cfg(unix)]
use std::os::unix::process::CommandExt;

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use tool_runtime_core::{
    credential_lifecycle_coordinator::{
        CredentialLifecycleCoordinator, CredentialLifecycleCoordinatorInstant,
        CredentialLifecycleLease, CredentialLifecycleLeaseCompletion,
        CredentialLifecycleLeaseState, CredentialLifecyclePendingKind,
        CredentialLifecycleTerminalOutcome,
    },
    credential_lifecycle_execution::{
        CredentialLifecycleBoundProcess, CredentialLifecycleExecutionBindingError,
        CredentialLifecycleInteractionAction, CredentialLifecycleInteractionBridge,
        CredentialLifecycleInvocation, CredentialLifecycleOutputChannel,
        CredentialLifecycleSensitiveOutput,
    },
    credential_lifecycle_observation::{
        evaluate_lifecycle_status, CredentialLifecycleCommandObservation,
        CredentialLifecycleObservationError, CredentialLifecycleStatusResult,
        CredentialLifecycleTermination, MAX_LIFECYCLE_OBSERVATION_STDERR_BYTES,
        MAX_LIFECYCLE_OBSERVATION_STDOUT_BYTES, MAX_LIFECYCLE_OBSERVATION_TOTAL_BYTES,
    },
    credential_materialization::CredentialRedactedOutput,
    manifest::CliInteraction,
};

const DEFAULT_LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(30);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(20);
const PROCESS_TERMINATION_GRACE: Duration = Duration::from_millis(100);
const PROCESS_READER_DRAIN_GRACE: Duration = Duration::from_millis(250);
const PROCESS_READER_STOP_GRACE: Duration = Duration::from_millis(100);
const PROCESS_STREAM_CHUNK_BYTES: usize = 8 * 1024;
const PROCESS_STREAM_CHANNEL_DEPTH: usize = 32;
const MAX_INTERACTION_TRANSITIONS: u32 = 4096;
const MAX_CONCURRENT_LIFECYCLE_PROCESSES: usize = 16;
static ACTIVE_LIFECYCLE_PROCESSES: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GovernedLifecycleExecutorErrorCode {
    BindingFailed,
    SpawnFailed,
    StreamUnavailable,
    StreamReadFailed,
    ProcessWaitFailed,
    OutputLimitExceeded,
    InteractionBridgeFailed,
    InteractionUnavailable,
    InteractionWriteFailed,
    InteractionCancelled,
    InteractionLimitExceeded,
    CoordinatorFailed,
    PlanMismatch,
    ObservationFailed,
    CapacityExceeded,
}

/// Fixed, secret-free executor diagnostic. Child output, argv, paths, and bridge values
/// never enter this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GovernedLifecycleExecutorError {
    pub code: GovernedLifecycleExecutorErrorCode,
    pub field: &'static str,
    pub message: &'static str,
}

impl GovernedLifecycleExecutorError {
    const fn new(
        code: GovernedLifecycleExecutorErrorCode,
        field: &'static str,
        message: &'static str,
    ) -> Self {
        Self {
            code,
            field,
            message,
        }
    }
}

impl fmt::Display for GovernedLifecycleExecutorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for GovernedLifecycleExecutorError {}

#[derive(Clone, Default)]
pub struct CredentialLifecycleProcessCancellation {
    cancelled: Arc<AtomicBool>,
}

impl CredentialLifecycleProcessCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

impl fmt::Debug for CredentialLifecycleProcessCancellation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialLifecycleProcessCancellation")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

pub struct GovernedCredentialLifecycleResult {
    termination: CredentialLifecycleTermination,
    stdout: CredentialRedactedOutput,
    stderr: CredentialRedactedOutput,
    last_pending: Option<CredentialLifecyclePendingKind>,
    interaction_transitions: u32,
}

impl fmt::Debug for GovernedCredentialLifecycleResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GovernedCredentialLifecycleResult")
            .field("termination", &self.termination)
            .field("stdout_bytes", &self.stdout.as_bytes().len())
            .field("stderr_bytes", &self.stderr.as_bytes().len())
            .field("last_pending", &self.last_pending)
            .field("interaction_transitions", &self.interaction_transitions)
            .finish()
    }
}

impl GovernedCredentialLifecycleResult {
    pub fn termination(&self) -> CredentialLifecycleTermination {
        self.termination
    }

    pub fn stdout(&self) -> &[u8] {
        self.stdout.as_bytes()
    }

    pub fn stderr(&self) -> &[u8] {
        self.stderr.as_bytes()
    }

    pub fn last_pending(&self) -> Option<CredentialLifecyclePendingKind> {
        self.last_pending
    }
}

pub struct GovernedCredentialLifecycleExecutor;

impl GovernedCredentialLifecycleExecutor {
    pub fn execute(
        invocation: &CredentialLifecycleInvocation,
        cancellation: &CredentialLifecycleProcessCancellation,
        bridge: Option<&mut dyn CredentialLifecycleInteractionBridge>,
    ) -> Result<GovernedCredentialLifecycleResult, GovernedLifecycleExecutorError> {
        let _permit = LifecycleProcessPermit::acquire()?;
        invocation
            .with_bound_process(|bound| execute_bound(bound, cancellation, bridge))
            .map_err(binding_failed)?
    }

    pub fn execute_status_for_plan(
        plan: &tool_runtime_core::credential_lifecycle::CredentialLifecyclePlan,
        invocation: &CredentialLifecycleInvocation,
        cancellation: &CredentialLifecycleProcessCancellation,
    ) -> Result<
        (
            GovernedCredentialLifecycleResult,
            CredentialLifecycleStatusResult,
        ),
        GovernedLifecycleExecutorError,
    > {
        if !invocation.matches_plan(plan) {
            return Err(plan_mismatch());
        }
        let result = Self::execute(invocation, cancellation, None)?;
        let observation = CredentialLifecycleCommandObservation::new(
            plan.operation(),
            result.termination(),
            result.stdout(),
            result.stderr(),
        )
        .map_err(observation_failed)?;
        let status = evaluate_lifecycle_status(plan, &observation).map_err(observation_failed)?;
        Ok((result, status))
    }

    pub fn execute_login(
        invocation: &CredentialLifecycleInvocation,
        coordinator: &CredentialLifecycleCoordinator,
        lease: CredentialLifecycleLease,
        cancellation: &CredentialLifecycleProcessCancellation,
        bridge: &mut dyn CredentialLifecycleInteractionBridge,
    ) -> Result<
        (
            GovernedCredentialLifecycleResult,
            CredentialLifecycleLeaseCompletion,
        ),
        GovernedLifecycleExecutorError,
    > {
        if !invocation.matches_plan(lease.plan()) {
            let _ = coordinator.complete(
                &lease,
                CredentialLifecycleTerminalOutcome::ProcessFailed,
                CredentialLifecycleCoordinatorInstant::now(),
            );
            return Err(plan_mismatch());
        }
        let snapshot = coordinator
            .snapshot(&lease, CredentialLifecycleCoordinatorInstant::now())
            .map_err(|_| coordinator_failed())?;
        let mut coordinated = CoordinatedInteractionBridge {
            inner: bridge,
            coordinator,
            lease: &lease,
            revision: snapshot.revision,
            state: snapshot.state,
        };
        let executed = Self::execute(invocation, cancellation, Some(&mut coordinated));
        drop(coordinated);
        let outcome = match &executed {
            Ok(result) => terminal_outcome(result.termination()),
            Err(error)
                if cancellation.is_cancelled()
                    || error.code == GovernedLifecycleExecutorErrorCode::InteractionCancelled =>
            {
                CredentialLifecycleTerminalOutcome::Cancelled
            },
            Err(_) => CredentialLifecycleTerminalOutcome::ProcessFailed,
        };
        let completion = coordinator
            .complete(
                &lease,
                outcome,
                CredentialLifecycleCoordinatorInstant::now(),
            )
            .map_err(|_| coordinator_failed())?;
        executed.map(|result| (result, completion))
    }
}

struct CoordinatedInteractionBridge<'a> {
    inner: &'a mut dyn CredentialLifecycleInteractionBridge,
    coordinator: &'a CredentialLifecycleCoordinator,
    lease: &'a CredentialLifecycleLease,
    revision: u64,
    state: CredentialLifecycleLeaseState,
}

impl CoordinatedInteractionBridge<'_> {
    fn coordinate(
        &mut self,
        action: CredentialLifecycleInteractionAction,
    ) -> Result<CredentialLifecycleInteractionAction, CredentialLifecycleExecutionBindingError>
    {
        let pending = match &action {
            CredentialLifecycleInteractionAction::Await { pending }
            | CredentialLifecycleInteractionAction::ProvideInput { pending, .. } => Some(*pending),
            CredentialLifecycleInteractionAction::Continue
            | CredentialLifecycleInteractionAction::Cancel => None,
        };
        if let Some(pending) = pending {
            if self.state != CredentialLifecycleLeaseState::Pending(pending) {
                let snapshot = self
                    .coordinator
                    .publish_pending(
                        self.lease,
                        self.revision,
                        pending,
                        CredentialLifecycleCoordinatorInstant::now(),
                    )
                    .map_err(|_| bridge_coordinator_binding_error())?;
                self.revision = snapshot.revision;
                self.state = snapshot.state;
            }
            if matches!(
                action,
                CredentialLifecycleInteractionAction::ProvideInput { .. }
            ) {
                let snapshot = self
                    .coordinator
                    .resume(
                        self.lease,
                        self.revision,
                        CredentialLifecycleCoordinatorInstant::now(),
                    )
                    .map_err(|_| bridge_coordinator_binding_error())?;
                self.revision = snapshot.revision;
                self.state = snapshot.state;
            }
        } else if matches!(action, CredentialLifecycleInteractionAction::Cancel)
            && !matches!(self.state, CredentialLifecycleLeaseState::Cancelling)
        {
            let snapshot = self
                .coordinator
                .request_cancel(self.lease, CredentialLifecycleCoordinatorInstant::now())
                .map_err(|_| bridge_coordinator_binding_error())?;
            self.revision = snapshot.revision;
            self.state = snapshot.state;
        }
        Ok(action)
    }
}

impl CredentialLifecycleInteractionBridge for CoordinatedInteractionBridge<'_> {
    fn on_output(
        &mut self,
        output: CredentialLifecycleSensitiveOutput<'_>,
    ) -> Result<CredentialLifecycleInteractionAction, CredentialLifecycleExecutionBindingError>
    {
        let action = self.inner.on_output(output)?;
        self.coordinate(action)
    }

    fn on_idle(
        &mut self,
    ) -> Result<CredentialLifecycleInteractionAction, CredentialLifecycleExecutionBindingError>
    {
        let action = self.inner.on_idle()?;
        self.coordinate(action)
    }
}

fn execute_bound(
    bound: CredentialLifecycleBoundProcess<'_>,
    cancellation: &CredentialLifecycleProcessCancellation,
    bridge: Option<&mut dyn CredentialLifecycleInteractionBridge>,
) -> Result<GovernedCredentialLifecycleResult, GovernedLifecycleExecutorError> {
    match bound.interaction() {
        // A bridge with a batch hook is a contract that declares login prompts
        // without asking for a PTY. `Batch` is the DEFAULT interaction, so that
        // combination is easy to write: the hook then runs with a null stdin,
        // blocks on its own prompt until the timeout, and the caller reports a
        // flat "login failed" — the operator is never told a credential was
        // wanted and the material they supplied is never used. Say so instead.
        CliInteraction::Batch if bridge.is_some() => Err(interaction_unavailable()),
        CliInteraction::Batch => execute_batch(bound, cancellation),
        CliInteraction::Pty => execute_pty(bound, cancellation, bridge),
    }
}

fn execute_batch(
    bound: CredentialLifecycleBoundProcess<'_>,
    cancellation: &CredentialLifecycleProcessCancellation,
) -> Result<GovernedCredentialLifecycleResult, GovernedLifecycleExecutorError> {
    let executable = bound.launch_executable().map_err(binding_failed)?;
    let mut command = Command::new(executable.as_path());
    command
        .args(bound.args())
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);
    apply_environment_to_command(&bound, &mut command)?;
    let child = command.spawn().map_err(|_| spawn_failed())?;
    let mut tree = ProcessTreeGuard::new(child);
    let stdout = tree
        .child_mut()?
        .stdout
        .take()
        .ok_or_else(stream_unavailable)?;
    let stderr = tree
        .child_mut()?
        .stderr
        .take()
        .ok_or_else(stream_unavailable)?;
    let (sender, receiver) = mpsc::sync_channel(PROCESS_STREAM_CHANNEL_DEPTH);
    #[cfg(unix)]
    let stdout_fd = Some(stdout.as_raw_fd());
    #[cfg(not(unix))]
    let stdout_fd = None;
    #[cfg(unix)]
    let stderr_fd = Some(stderr.as_raw_fd());
    #[cfg(not(unix))]
    let stderr_fd = None;
    let stdout_reader = spawn_stream_reader(
        stdout,
        stdout_fd,
        CredentialLifecycleOutputChannel::Stdout,
        sender.clone(),
    );
    let stderr_reader = spawn_stream_reader(
        stderr,
        stderr_fd,
        CredentialLifecycleOutputChannel::Stderr,
        sender,
    );
    let timeout = timeout_for(&bound);
    let raw = collect_batch(
        &mut tree,
        receiver,
        [stdout_reader, stderr_reader],
        cancellation,
        timeout,
    )?;
    seal_result(&bound, raw)
}

fn execute_pty(
    bound: CredentialLifecycleBoundProcess<'_>,
    cancellation: &CredentialLifecycleProcessCancellation,
    bridge: Option<&mut dyn CredentialLifecycleInteractionBridge>,
) -> Result<GovernedCredentialLifecycleResult, GovernedLifecycleExecutorError> {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 32,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|_| spawn_failed())?;
    let executable = bound.launch_executable().map_err(binding_failed)?;
    let mut command = CommandBuilder::new(executable.as_path());
    command.env_clear();
    for argument in bound.args() {
        command.arg(argument);
    }
    apply_environment_to_pty(&bound, &mut command)?;
    let child = pair
        .slave
        .spawn_command(command)
        .map_err(|_| spawn_failed())?;
    drop(pair.slave);
    let pid = child.process_id();
    let mut tree = PtyProcessTreeGuard::new(child, pid, pair.master);
    let reader = tree.try_clone_reader()?;
    let reader_fd = tree.reader_fd();
    let writer = tree.take_writer()?;
    let (sender, receiver) = mpsc::sync_channel(PROCESS_STREAM_CHANNEL_DEPTH);
    let reader = spawn_stream_reader(
        reader,
        reader_fd,
        CredentialLifecycleOutputChannel::Pty,
        sender,
    );
    let timeout = timeout_for(&bound);
    let raw = collect_pty(
        &mut tree,
        writer,
        receiver,
        reader,
        cancellation,
        timeout,
        bridge,
    )?;
    seal_result(&bound, raw)
}

fn apply_environment_to_command(
    bound: &CredentialLifecycleBoundProcess<'_>,
    command: &mut Command,
) -> Result<(), GovernedLifecycleExecutorError> {
    for index in 0..bound.environment_len() {
        bound
            .with_environment_entry(index, |name, value| {
                command.env(name, value);
            })
            .map_err(binding_failed)?
            .ok_or_else(binding_failed_missing_environment)?;
    }
    Ok(())
}

fn apply_environment_to_pty(
    bound: &CredentialLifecycleBoundProcess<'_>,
    command: &mut CommandBuilder,
) -> Result<(), GovernedLifecycleExecutorError> {
    for index in 0..bound.environment_len() {
        bound
            .with_environment_entry(index, |name, value| command.env(name, value))
            .map_err(binding_failed)?
            .ok_or_else(binding_failed_missing_environment)?;
    }
    Ok(())
}

enum StreamEvent {
    Chunk(CredentialLifecycleOutputChannel, Vec<u8>),
    Done,
    Failed,
}

struct StreamReaderHandle {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

fn spawn_stream_reader(
    mut reader: impl Read + Send + 'static,
    #[cfg(unix)] raw_fd: Option<RawFd>,
    #[cfg(not(unix))] _raw_fd: Option<i32>,
    channel: CredentialLifecycleOutputChannel,
    sender: SyncSender<StreamEvent>,
) -> StreamReaderHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread = thread::spawn(move || {
        let mut buffer = [0_u8; PROCESS_STREAM_CHUNK_BYTES];
        while !thread_stop.load(Ordering::Acquire) {
            #[cfg(unix)]
            if let Some(raw_fd) = raw_fd {
                let mut descriptor = libc::pollfd {
                    fd: raw_fd,
                    events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
                    revents: 0,
                };
                // SAFETY: the reader owns the descriptor for the lifetime of this thread.
                let ready = unsafe {
                    libc::poll(
                        &mut descriptor,
                        1,
                        i32::try_from(PROCESS_POLL_INTERVAL.as_millis()).unwrap_or(20),
                    )
                };
                if ready == 0 {
                    continue;
                }
                if ready < 0 {
                    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    send_stream_event(&sender, StreamEvent::Failed, &thread_stop);
                    break;
                }
            }
            match reader.read(&mut buffer) {
                Ok(0) => {
                    send_stream_event(&sender, StreamEvent::Done, &thread_stop);
                    break;
                },
                Ok(count) => {
                    if !send_stream_event(
                        &sender,
                        StreamEvent::Chunk(channel, buffer[..count].to_vec()),
                        &thread_stop,
                    ) {
                        break;
                    }
                },
                Err(error) => {
                    #[cfg(unix)]
                    let event = if error.raw_os_error() == Some(libc::EIO) {
                        StreamEvent::Done
                    } else {
                        StreamEvent::Failed
                    };
                    #[cfg(not(unix))]
                    let event = StreamEvent::Failed;
                    send_stream_event(&sender, event, &thread_stop);
                    break;
                },
            }
        }
    });
    StreamReaderHandle { stop, thread }
}

fn send_stream_event(
    sender: &SyncSender<StreamEvent>,
    mut event: StreamEvent,
    stop: &AtomicBool,
) -> bool {
    loop {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        match sender.try_send(event) {
            Ok(()) => return true,
            Err(TrySendError::Full(returned)) => {
                event = returned;
                thread::yield_now();
            },
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
}

struct RawProcessOutput {
    termination: CredentialLifecycleTermination,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    last_pending: Option<CredentialLifecyclePendingKind>,
    interaction_transitions: u32,
}

fn collect_batch(
    tree: &mut ProcessTreeGuard,
    receiver: Receiver<StreamEvent>,
    readers: [StreamReaderHandle; 2],
    cancellation: &CredentialLifecycleProcessCancellation,
    timeout: Duration,
) -> Result<RawProcessOutput, GovernedLifecycleExecutorError> {
    let started = Instant::now();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let outcome = loop {
        if cancellation.is_cancelled() {
            tree.terminate_and_reap();
            break Ok(CredentialLifecycleTermination::Cancelled);
        }
        if started.elapsed() >= timeout {
            tree.terminate_and_reap();
            break Ok(CredentialLifecycleTermination::TimedOut);
        }
        match receiver.recv_timeout(PROCESS_POLL_INTERVAL) {
            Ok(StreamEvent::Chunk(channel, bytes)) => {
                if let Err(error) = append_output(&mut stdout, &mut stderr, channel, &bytes) {
                    tree.terminate_and_reap();
                    break Err(error);
                }
            },
            Ok(StreamEvent::Done) => {},
            Ok(StreamEvent::Failed) => {
                tree.terminate_and_reap();
                break Err(stream_read_failed());
            },
            Err(RecvTimeoutError::Timeout) => {},
            Err(RecvTimeoutError::Disconnected) => {},
        }
        match tree.try_wait() {
            Ok(Some(status)) => {
                // The launcher may have left descendants holding the pipes. Its process group
                // remains this call's authority, so terminate any remainder before joining.
                tree.terminate_group_only();
                tree.mark_reaped();
                break Ok(termination_from_status(status));
            },
            Ok(None) => {},
            Err(error) => {
                tree.terminate_and_reap();
                break Err(error);
            },
        }
    };
    let readers_finished = finish_readers(readers, &receiver, &mut stdout, &mut stderr);
    let termination = combine_process_and_reader_outcomes(outcome, readers_finished)?;
    Ok(RawProcessOutput {
        termination,
        stdout,
        stderr,
        last_pending: None,
        interaction_transitions: 0,
    })
}

fn collect_pty(
    tree: &mut PtyProcessTreeGuard,
    mut writer: Box<dyn Write + Send>,
    receiver: Receiver<StreamEvent>,
    reader: StreamReaderHandle,
    cancellation: &CredentialLifecycleProcessCancellation,
    timeout: Duration,
    mut bridge: Option<&mut dyn CredentialLifecycleInteractionBridge>,
) -> Result<RawProcessOutput, GovernedLifecycleExecutorError> {
    let started = Instant::now();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut last_pending = None;
    let mut transitions = 0_u32;
    let outcome = loop {
        if cancellation.is_cancelled() {
            tree.terminate_and_reap();
            break Ok(CredentialLifecycleTermination::Cancelled);
        }
        if started.elapsed() >= timeout {
            tree.terminate_and_reap();
            break Ok(CredentialLifecycleTermination::TimedOut);
        }
        match receiver.recv_timeout(PROCESS_POLL_INTERVAL) {
            Ok(StreamEvent::Chunk(_, bytes)) => {
                if let Err(error) = append_output(
                    &mut stdout,
                    &mut stderr,
                    CredentialLifecycleOutputChannel::Pty,
                    &bytes,
                ) {
                    tree.terminate_and_reap();
                    break Err(error);
                }
                if let Some(bridge) = bridge.as_deref_mut() {
                    let action = match bridge.on_output(CredentialLifecycleSensitiveOutput::new(
                        CredentialLifecycleOutputChannel::Pty,
                        &bytes,
                    )) {
                        Ok(action) => action,
                        Err(_) => {
                            tree.terminate_and_reap();
                            break Err(interaction_bridge_failed());
                        },
                    };
                    if let Err(error) = apply_interaction_action(
                        action,
                        &mut writer,
                        &mut last_pending,
                        &mut transitions,
                    ) {
                        tree.terminate_and_reap();
                        break Err(error);
                    }
                }
            },
            Ok(StreamEvent::Done) => {},
            Ok(StreamEvent::Failed) => {
                tree.terminate_and_reap();
                break Err(stream_read_failed());
            },
            Err(RecvTimeoutError::Timeout) => {
                if let Some(bridge) = bridge.as_deref_mut() {
                    let action = match bridge.on_idle() {
                        Ok(action) => action,
                        Err(_) => {
                            tree.terminate_and_reap();
                            break Err(interaction_bridge_failed());
                        },
                    };
                    if let Err(error) = apply_interaction_action(
                        action,
                        &mut writer,
                        &mut last_pending,
                        &mut transitions,
                    ) {
                        tree.terminate_and_reap();
                        break Err(error);
                    }
                }
            },
            Err(RecvTimeoutError::Disconnected) => {},
        }
        match tree.try_wait() {
            Ok(Some(status)) => {
                tree.terminate_group_only();
                tree.mark_reaped();
                break Ok(if status.success() {
                    CredentialLifecycleTermination::Exited { code: 0 }
                } else if status.signal().is_some() {
                    CredentialLifecycleTermination::Signaled
                } else {
                    let code = status.exit_code();
                    if (0..=u8::MAX as u32).contains(&code) {
                        CredentialLifecycleTermination::Exited { code: code as u8 }
                    } else {
                        CredentialLifecycleTermination::Signaled
                    }
                });
            },
            Ok(None) => {},
            Err(error) => {
                tree.terminate_and_reap();
                break Err(error);
            },
        }
    };
    drop(writer);
    let readers_finished = finish_readers([reader], &receiver, &mut stdout, &mut stderr);
    tree.close_master();
    let termination = combine_process_and_reader_outcomes(outcome, readers_finished)?;
    Ok(RawProcessOutput {
        termination,
        stdout,
        stderr,
        last_pending,
        interaction_transitions: transitions,
    })
}

fn apply_interaction_action(
    action: CredentialLifecycleInteractionAction,
    writer: &mut Box<dyn Write + Send>,
    last_pending: &mut Option<CredentialLifecyclePendingKind>,
    transitions: &mut u32,
) -> Result<(), GovernedLifecycleExecutorError> {
    match action {
        CredentialLifecycleInteractionAction::Continue => Ok(()),
        CredentialLifecycleInteractionAction::Await { pending } => {
            if *last_pending == Some(pending) {
                return Ok(());
            }
            *last_pending = Some(pending);
            advance_interaction(transitions)
        },
        CredentialLifecycleInteractionAction::ProvideInput { pending, input } => {
            *last_pending = Some(pending);
            advance_interaction(transitions)?;
            input
                .with_bytes(|bytes| writer.write_all(bytes))
                .map_err(|_| interaction_write_failed())?;
            writer.flush().map_err(|_| interaction_write_failed())
        },
        CredentialLifecycleInteractionAction::Cancel => Err(interaction_cancelled()),
    }
}

fn advance_interaction(transitions: &mut u32) -> Result<(), GovernedLifecycleExecutorError> {
    *transitions = transitions
        .checked_add(1)
        .ok_or_else(interaction_limit_exceeded)?;
    if *transitions > MAX_INTERACTION_TRANSITIONS {
        return Err(interaction_limit_exceeded());
    }
    Ok(())
}

fn append_output(
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
    channel: CredentialLifecycleOutputChannel,
    bytes: &[u8],
) -> Result<(), GovernedLifecycleExecutorError> {
    let stdout_len = stdout.len();
    let stderr_len = stderr.len();
    let per_stream_limit = match channel {
        CredentialLifecycleOutputChannel::Stdout | CredentialLifecycleOutputChannel::Pty => {
            MAX_LIFECYCLE_OBSERVATION_STDOUT_BYTES
        },
        CredentialLifecycleOutputChannel::Stderr => MAX_LIFECYCLE_OBSERVATION_STDERR_BYTES,
    };
    let current_stream = match channel {
        CredentialLifecycleOutputChannel::Stdout | CredentialLifecycleOutputChannel::Pty => {
            stdout_len
        },
        CredentialLifecycleOutputChannel::Stderr => stderr_len,
    };
    let next_stream = current_stream
        .checked_add(bytes.len())
        .ok_or_else(output_limit_exceeded)?;
    let next_total = stdout_len
        .checked_add(stderr_len)
        .and_then(|value| value.checked_add(bytes.len()))
        .ok_or_else(output_limit_exceeded)?;
    if next_stream > per_stream_limit || next_total > MAX_LIFECYCLE_OBSERVATION_TOTAL_BYTES {
        return Err(output_limit_exceeded());
    }
    match channel {
        CredentialLifecycleOutputChannel::Stdout | CredentialLifecycleOutputChannel::Pty => {
            stdout.extend_from_slice(bytes);
        },
        CredentialLifecycleOutputChannel::Stderr => stderr.extend_from_slice(bytes),
    }
    Ok(())
}

fn finish_readers<const N: usize>(
    readers: [StreamReaderHandle; N],
    receiver: &Receiver<StreamEvent>,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
) -> Result<(), GovernedLifecycleExecutorError> {
    let mut first_error = None;
    let drain_deadline = Instant::now() + PROCESS_READER_DRAIN_GRACE;
    while readers.iter().any(|reader| !reader.thread.is_finished())
        && Instant::now() < drain_deadline
    {
        match receiver.recv_timeout(PROCESS_POLL_INTERVAL) {
            Ok(StreamEvent::Chunk(channel, bytes)) => {
                record_reader_event(
                    StreamEvent::Chunk(channel, bytes),
                    stdout,
                    stderr,
                    &mut first_error,
                );
            },
            Ok(StreamEvent::Done) => {},
            Ok(StreamEvent::Failed) => {
                first_error.get_or_insert_with(stream_read_failed);
            },
            Err(RecvTimeoutError::Timeout) => {},
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    while let Ok(event) = receiver.try_recv() {
        record_reader_event(event, stdout, stderr, &mut first_error);
    }
    for reader in &readers {
        reader.stop.store(true, Ordering::Release);
    }
    let stop_deadline = Instant::now() + PROCESS_READER_STOP_GRACE;
    while readers.iter().any(|reader| !reader.thread.is_finished())
        && Instant::now() < stop_deadline
    {
        while let Ok(event) = receiver.try_recv() {
            record_reader_event(event, stdout, stderr, &mut first_error);
        }
        thread::sleep(
            PROCESS_POLL_INTERVAL.min(stop_deadline.saturating_duration_since(Instant::now())),
        );
    }
    for reader in readers {
        if !reader.thread.is_finished() {
            first_error.get_or_insert_with(stream_read_failed);
            continue;
        }
        if reader.thread.join().is_err() {
            first_error.get_or_insert_with(stream_read_failed);
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn record_reader_event(
    event: StreamEvent,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
    first_error: &mut Option<GovernedLifecycleExecutorError>,
) {
    match event {
        StreamEvent::Chunk(channel, bytes) if first_error.is_none() => {
            if let Err(error) = append_output(stdout, stderr, channel, &bytes) {
                *first_error = Some(error);
            }
        },
        StreamEvent::Chunk(_, _) | StreamEvent::Done => {},
        StreamEvent::Failed => {
            first_error.get_or_insert_with(stream_read_failed);
        },
    }
}

fn combine_process_and_reader_outcomes(
    process: Result<CredentialLifecycleTermination, GovernedLifecycleExecutorError>,
    readers: Result<(), GovernedLifecycleExecutorError>,
) -> Result<CredentialLifecycleTermination, GovernedLifecycleExecutorError> {
    match (process, readers) {
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        (Ok(termination), Ok(())) => Ok(termination),
    }
}

fn seal_result(
    bound: &CredentialLifecycleBoundProcess<'_>,
    raw: RawProcessOutput,
) -> Result<GovernedCredentialLifecycleResult, GovernedLifecycleExecutorError> {
    Ok(GovernedCredentialLifecycleResult {
        termination: raw.termination,
        stdout: bound.redact_output(&raw.stdout).map_err(binding_failed)?,
        stderr: bound.redact_output(&raw.stderr).map_err(binding_failed)?,
        last_pending: raw.last_pending,
        interaction_transitions: raw.interaction_transitions,
    })
}

struct ProcessTreeGuard {
    child: Option<Child>,
    pid: Option<u32>,
}

struct LifecycleProcessPermit;

impl LifecycleProcessPermit {
    fn acquire() -> Result<Self, GovernedLifecycleExecutorError> {
        ACTIVE_LIFECYCLE_PROCESSES
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_CONCURRENT_LIFECYCLE_PROCESSES).then_some(active + 1)
            })
            .map(|_| Self)
            .map_err(|_| capacity_exceeded())
    }
}

impl Drop for LifecycleProcessPermit {
    fn drop(&mut self) {
        ACTIVE_LIFECYCLE_PROCESSES.fetch_sub(1, Ordering::AcqRel);
    }
}

impl ProcessTreeGuard {
    fn new(child: Child) -> Self {
        let pid = Some(child.id());
        Self {
            child: Some(child),
            pid,
        }
    }

    fn child_mut(&mut self) -> Result<&mut Child, GovernedLifecycleExecutorError> {
        self.child.as_mut().ok_or_else(process_wait_failed)
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>, GovernedLifecycleExecutorError> {
        self.child_mut()?
            .try_wait()
            .map_err(|_| process_wait_failed())
    }

    fn terminate_group_only(&self) {
        terminate_process_group(self.pid);
    }

    fn mark_reaped(&mut self) {
        self.child = None;
    }

    fn terminate_and_reap(&mut self) {
        self.terminate_group_only();
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.child = None;
    }
}

impl Drop for ProcessTreeGuard {
    fn drop(&mut self) {
        if self.child.is_some() {
            self.terminate_and_reap();
        }
    }
}

struct PtyProcessTreeGuard {
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
    pid: Option<u32>,
    _master: Option<Box<dyn MasterPty + Send>>,
}

impl PtyProcessTreeGuard {
    fn new(
        child: Box<dyn portable_pty::Child + Send + Sync>,
        pid: Option<u32>,
        master: Box<dyn MasterPty + Send>,
    ) -> Self {
        Self {
            child: Some(child),
            pid,
            _master: Some(master),
        }
    }

    fn try_wait(
        &mut self,
    ) -> Result<Option<portable_pty::ExitStatus>, GovernedLifecycleExecutorError> {
        self.child
            .as_mut()
            .ok_or_else(process_wait_failed)?
            .try_wait()
            .map_err(|_| process_wait_failed())
    }

    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, GovernedLifecycleExecutorError> {
        self._master
            .as_ref()
            .ok_or_else(stream_unavailable)?
            .try_clone_reader()
            .map_err(|_| stream_unavailable())
    }

    fn take_writer(&self) -> Result<Box<dyn Write + Send>, GovernedLifecycleExecutorError> {
        self._master
            .as_ref()
            .ok_or_else(interaction_unavailable)?
            .take_writer()
            .map_err(|_| interaction_unavailable())
    }

    #[cfg(unix)]
    fn reader_fd(&self) -> Option<RawFd> {
        self._master.as_ref().and_then(|master| master.as_raw_fd())
    }

    #[cfg(not(unix))]
    fn reader_fd(&self) -> Option<i32> {
        None
    }

    fn close_master(&mut self) {
        self._master = None;
    }

    fn terminate_group_only(&self) {
        terminate_process_group(self.pid);
    }

    fn mark_reaped(&mut self) {
        self.child = None;
        self._master = None;
    }

    fn terminate_and_reap(&mut self) {
        self.terminate_group_only();
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.child = None;
        self._master = None;
    }
}

impl Drop for PtyProcessTreeGuard {
    fn drop(&mut self) {
        if self.child.is_some() {
            self.terminate_and_reap();
        }
    }
}

#[cfg(unix)]
fn terminate_process_group(pid: Option<u32>) {
    let Some(pid) = pid.and_then(|value| i32::try_from(value).ok()) else {
        return;
    };
    let process_group = -pid;
    // SAFETY: negative pids address only the isolated process group created for this
    // invocation. The group leader remains owned by its guard until it is reaped.
    let delivered = unsafe { libc::kill(process_group, libc::SIGTERM) } == 0;
    if !delivered {
        return;
    }
    thread::sleep(PROCESS_TERMINATION_GRACE);
    // SAFETY: same exact owned process group; SIGKILL is the bounded cleanup backstop.
    unsafe {
        libc::kill(process_group, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn terminate_process_group(_pid: Option<u32>) {}

fn timeout_for(bound: &CredentialLifecycleBoundProcess<'_>) -> Duration {
    bound
        .timeout_secs()
        .map(|seconds| Duration::from_secs(u64::from(seconds)))
        .unwrap_or(DEFAULT_LIFECYCLE_TIMEOUT)
}

fn termination_from_status(status: ExitStatus) -> CredentialLifecycleTermination {
    match status.code() {
        Some(code) if (0..=i32::from(u8::MAX)).contains(&code) => {
            CredentialLifecycleTermination::Exited { code: code as u8 }
        },
        _ => CredentialLifecycleTermination::Signaled,
    }
}

fn terminal_outcome(
    termination: CredentialLifecycleTermination,
) -> CredentialLifecycleTerminalOutcome {
    match termination {
        CredentialLifecycleTermination::Exited { code: 0 } => {
            CredentialLifecycleTerminalOutcome::Succeeded
        },
        CredentialLifecycleTermination::Cancelled => CredentialLifecycleTerminalOutcome::Cancelled,
        CredentialLifecycleTermination::TimedOut => CredentialLifecycleTerminalOutcome::TimedOut,
        CredentialLifecycleTermination::Exited { .. }
        | CredentialLifecycleTermination::Signaled
        | CredentialLifecycleTermination::SpawnFailed => {
            CredentialLifecycleTerminalOutcome::ProcessFailed
        },
    }
}

fn binding_failed(
    _error: CredentialLifecycleExecutionBindingError,
) -> GovernedLifecycleExecutorError {
    GovernedLifecycleExecutorError::new(
        GovernedLifecycleExecutorErrorCode::BindingFailed,
        "credential_lifecycle.binding",
        "the sealed lifecycle invocation could not be revalidated",
    )
}

const fn binding_failed_missing_environment() -> GovernedLifecycleExecutorError {
    binding_failed_fixed()
}

const fn binding_failed_fixed() -> GovernedLifecycleExecutorError {
    GovernedLifecycleExecutorError::new(
        GovernedLifecycleExecutorErrorCode::BindingFailed,
        "credential_lifecycle.environment",
        "the sealed lifecycle environment changed before process spawn",
    )
}

const fn spawn_failed() -> GovernedLifecycleExecutorError {
    GovernedLifecycleExecutorError::new(
        GovernedLifecycleExecutorErrorCode::SpawnFailed,
        "credential_lifecycle.process",
        "the governed lifecycle process could not be started",
    )
}

const fn stream_unavailable() -> GovernedLifecycleExecutorError {
    GovernedLifecycleExecutorError::new(
        GovernedLifecycleExecutorErrorCode::StreamUnavailable,
        "credential_lifecycle.stream",
        "the governed lifecycle output stream is unavailable",
    )
}

const fn stream_read_failed() -> GovernedLifecycleExecutorError {
    GovernedLifecycleExecutorError::new(
        GovernedLifecycleExecutorErrorCode::StreamReadFailed,
        "credential_lifecycle.stream",
        "the governed lifecycle output stream could not be read",
    )
}

const fn process_wait_failed() -> GovernedLifecycleExecutorError {
    GovernedLifecycleExecutorError::new(
        GovernedLifecycleExecutorErrorCode::ProcessWaitFailed,
        "credential_lifecycle.process",
        "the governed lifecycle process could not be reaped",
    )
}

const fn capacity_exceeded() -> GovernedLifecycleExecutorError {
    GovernedLifecycleExecutorError::new(
        GovernedLifecycleExecutorErrorCode::CapacityExceeded,
        "credential_lifecycle.capacity",
        "the governed lifecycle process limit is already in use",
    )
}

const fn output_limit_exceeded() -> GovernedLifecycleExecutorError {
    GovernedLifecycleExecutorError::new(
        GovernedLifecycleExecutorErrorCode::OutputLimitExceeded,
        "credential_lifecycle.output",
        "the governed lifecycle process exceeded its output limit",
    )
}

const fn interaction_bridge_failed() -> GovernedLifecycleExecutorError {
    GovernedLifecycleExecutorError::new(
        GovernedLifecycleExecutorErrorCode::InteractionBridgeFailed,
        "credential_lifecycle.interaction",
        "the product credential interaction bridge failed closed",
    )
}

const fn interaction_unavailable() -> GovernedLifecycleExecutorError {
    GovernedLifecycleExecutorError::new(
        GovernedLifecycleExecutorErrorCode::InteractionUnavailable,
        "credential_lifecycle.interaction",
        "the declared interactive lifecycle stream is unavailable",
    )
}

const fn interaction_write_failed() -> GovernedLifecycleExecutorError {
    GovernedLifecycleExecutorError::new(
        GovernedLifecycleExecutorErrorCode::InteractionWriteFailed,
        "credential_lifecycle.interaction",
        "the governed lifecycle input could not be delivered",
    )
}

const fn interaction_cancelled() -> GovernedLifecycleExecutorError {
    GovernedLifecycleExecutorError::new(
        GovernedLifecycleExecutorErrorCode::InteractionCancelled,
        "credential_lifecycle.interaction",
        "the product credential interaction bridge cancelled the lifecycle",
    )
}

const fn interaction_limit_exceeded() -> GovernedLifecycleExecutorError {
    GovernedLifecycleExecutorError::new(
        GovernedLifecycleExecutorErrorCode::InteractionLimitExceeded,
        "credential_lifecycle.interaction",
        "the governed lifecycle exceeded its interaction transition limit",
    )
}

const fn coordinator_failed() -> GovernedLifecycleExecutorError {
    GovernedLifecycleExecutorError::new(
        GovernedLifecycleExecutorErrorCode::CoordinatorFailed,
        "credential_lifecycle.coordinator",
        "the interactive lifecycle coordinator rejected the execution transition",
    )
}

const fn plan_mismatch() -> GovernedLifecycleExecutorError {
    GovernedLifecycleExecutorError::new(
        GovernedLifecycleExecutorErrorCode::PlanMismatch,
        "credential_lifecycle.plan",
        "the lifecycle invocation does not match the coordinator or observation plan",
    )
}

fn observation_failed(
    _error: CredentialLifecycleObservationError,
) -> GovernedLifecycleExecutorError {
    GovernedLifecycleExecutorError::new(
        GovernedLifecycleExecutorErrorCode::ObservationFailed,
        "credential_lifecycle.observation",
        "the governed lifecycle output did not satisfy its declared status contract",
    )
}

const fn bridge_coordinator_binding_error() -> CredentialLifecycleExecutionBindingError {
    CredentialLifecycleExecutionBindingError {
        code: tool_runtime_core::credential_lifecycle_execution::CredentialLifecycleExecutionBindingErrorCode::PlanMismatch,
        field: "lifecycle_execution.interaction",
        message: "the product interaction no longer owns the exact lifecycle lease",
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::{
        collections::BTreeSet,
        fs,
        os::{
            fd::FromRawFd,
            unix::{ffi::OsStrExt, fs::PermissionsExt},
        },
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    use tool_runtime_core::{
        credential_injection::{ChildEnvironmentBaseline, ChildEnvironmentVariable},
        credential_lifecycle::{
            CredentialLifecycleErrorCode, CredentialLifecycleOperation, CredentialLifecyclePlan,
        },
        credential_lifecycle_coordinator::{
            CredentialLifecycleCoordinatorErrorCode, CredentialLifecycleIdleTimeout,
        },
        credential_lifecycle_execution::{
            CredentialLifecycleSensitiveInput, CredentialLifecycleSensitiveOutput,
        },
        credential_materialization::ChildEnvironmentValues,
        credential_profiles::{
            CreateCredentialProfileReference, CredentialProfileAvailability,
            CredentialProfileBinding, CredentialProfileError, CredentialProfileKey,
            CredentialProfileMetadata, CredentialProfileRegistry,
            CredentialProfileRegistrySnapshot, CredentialProfileRevision, CredentialProfileStatus,
            CredentialScope, ExpectedCredentialIdentity, SetCredentialProfileDisabled,
            UpdateCredentialProfileMetadata,
        },
        credential_status_cache::{
            CredentialPolicyRevision, CredentialProcessEpoch, CredentialVerifiedStatusCache,
        },
        manifest::{
            AuthContract, AuthKind, AuthLifecycle, AuthRequirement, AuthState, AuthStorage,
            IdentityContract, IdentitySelector, InjectionBinding, InjectionSource, InjectionTarget,
            LifecycleHook, LifecycleJsonPredicate, LifecycleJsonScalar, LifecycleObservedAuthState,
            LifecycleStatusObservation, LifecycleStatusOutputFormat, LifecycleStatusRule,
            ProfileSelection, RuntimeLimits, RuntimeProtocol, RuntimeRequirements,
            SkillRuntimeContract, SkillRuntimeContractVersion, StdinContract,
            WorkingDirectoryContract,
        },
        manifest_validation::validate_skill_runtime_contract,
        scoped_paths::{ScopedPath, ScopedPathAuthority, ScopedPathComponent},
    };

    use super::*;

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);
    static NEXT_EPOCH: AtomicU64 = AtomicU64::new(10_000);

    const FAKE_CLI: &str = r#"#!/bin/sh
operation="$1"
mode="$2"
case "$operation:$mode" in
  status:ready)
    printf '{"state":"ready","account":{"email":"work@example.com"}}'
    ;;
  status:missing)
    printf '{"state":"missing"}'
    ;;
  status:expired)
    printf '{"state":"expired"}'
    ;;
  status:revoked)
    printf '{"state":"revoked"}'
    ;;
  status:mismatched)
    printf '{"state":"ready","account":{"email":"other@example.com"}}'
    ;;
  status:malformed)
    printf '{not-json'
    ;;
  status:slow)
    /bin/sleep 30
    ;;
  status:flood)
    /bin/dd if=/dev/zero bs=1024 count=512 2>/dev/null
    ;;
  login:success)
    printf 'login-complete'
    ;;
  login:otp)
    printf 'otp-required:'
    IFS= read answer
    if [ "$answer" = "123456" ]; then
      printf 'login-complete'
      exit 0
    fi
    exit 9
    ;;
  *)
    exit 7
    ;;
esac
"#;

    struct TestRegistry {
        profiles: Vec<CredentialProfileStatus>,
    }

    impl CredentialProfileRegistry for TestRegistry {
        fn snapshot(
            &self,
            scope: &CredentialScope,
        ) -> Result<CredentialProfileRegistrySnapshot, CredentialProfileError> {
            CredentialProfileRegistrySnapshot::new(scope.clone(), self.profiles.clone())
        }

        fn status(
            &self,
            key: &CredentialProfileKey,
        ) -> Result<Option<CredentialProfileStatus>, CredentialProfileError> {
            Ok(self
                .profiles
                .iter()
                .find(|profile| profile.key() == key)
                .cloned())
        }

        fn create_reference(
            &self,
            _request: CreateCredentialProfileReference,
        ) -> Result<CredentialProfileStatus, CredentialProfileError> {
            Err(CredentialProfileError::registry_unavailable())
        }

        fn update_metadata(
            &self,
            _request: UpdateCredentialProfileMetadata,
        ) -> Result<CredentialProfileStatus, CredentialProfileError> {
            Err(CredentialProfileError::registry_unavailable())
        }

        fn set_disabled(
            &self,
            _request: SetCredentialProfileDisabled,
        ) -> Result<CredentialProfileStatus, CredentialProfileError> {
            Err(CredentialProfileError::registry_unavailable())
        }
    }

    struct Fixture {
        root: PathBuf,
        scopes_root: PathBuf,
        bin_root: PathBuf,
        scope: CredentialScope,
        key: CredentialProfileKey,
    }

    impl Fixture {
        fn new() -> Self {
            let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root = fs::canonicalize(std::env::temp_dir())
                .expect("temp root")
                .join(format!(
                    "magician-lifecycle-executor-{}-{sequence}",
                    std::process::id()
                ));
            let scopes_root = root.join("scopes");
            let bin_root = root.join("bin");
            let scope = CredentialScope::new("owner", "default").expect("scope");
            let key = CredentialProfileKey::new(
                scope.clone(),
                "google-workspace",
                "work",
                CredentialProfileBinding::Provider,
            )
            .expect("key");
            let workspace = scopes_root
                .join(scope.principal.as_str())
                .join(scope.workspace.as_str());
            fs::create_dir_all(workspace.join("auth").join("work")).expect("profile tree");
            fs::create_dir(&bin_root).expect("bin root");
            set_mode(&root, 0o700);
            set_mode(&scopes_root, 0o755);
            set_mode(&scopes_root.join(scope.principal.as_str()), 0o755);
            set_mode(&workspace, 0o755);
            set_mode(&workspace.join("auth"), 0o700);
            set_mode(&workspace.join("auth").join("work"), 0o700);
            set_mode(&bin_root, 0o700);
            let executable = bin_root.join("fake-cli");
            fs::write(&executable, FAKE_CLI).expect("fake CLI");
            set_mode(&executable, 0o755);
            Self {
                root,
                scopes_root,
                bin_root,
                scope,
                key,
            }
        }

        fn profile(&self, alias: &str, state: AuthState) -> CredentialProfileStatus {
            let key = CredentialProfileKey::new(
                self.scope.clone(),
                "google-workspace",
                alias,
                CredentialProfileBinding::Provider,
            )
            .expect("key");
            let metadata = CredentialProfileMetadata::new(
                key,
                Some(
                    ExpectedCredentialIdentity::new(format!("{alias}@example.com"))
                        .expect("identity"),
                ),
                alias == "work",
                CredentialProfileAvailability::Enabled,
                CredentialProfileRevision::new(7).expect("revision"),
            )
            .expect("metadata");
            CredentialProfileStatus::new(metadata, state).expect("status")
        }

        fn profile_root(&self) -> ScopedPath {
            ScopedPathAuthority::open(&self.scopes_root)
                .expect("authority")
                .resolve_profile_root(
                    &self.key,
                    ScopedPathComponent::new("work").expect("component"),
                )
                .expect("profile root")
        }

        fn environment(&self, baseline: &ChildEnvironmentBaseline) -> ChildEnvironmentValues {
            let mut values = ChildEnvironmentValues::new(baseline);
            values
                .provide(
                    ChildEnvironmentVariable::Path,
                    self.bin_root.as_os_str().as_bytes().to_vec(),
                )
                .expect("PATH");
            values
        }

        fn invocation(
            &self,
            contract: &SkillRuntimeContract,
            plan: CredentialLifecyclePlan,
        ) -> CredentialLifecycleInvocation {
            let validated = validate_skill_runtime_contract(contract).expect("contract");
            let baseline = ChildEnvironmentBaseline::portable_cli();
            CredentialLifecycleInvocation::bind(
                validated,
                plan,
                self.profile_root(),
                &baseline,
                self.environment(&baseline),
            )
            .expect("invocation")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn set_mode(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("mode");
    }

    fn status_rule(state: LifecycleObservedAuthState, value: &str) -> LifecycleStatusRule {
        LifecycleStatusRule {
            state,
            exit_codes: BTreeSet::from([0]),
            all: vec![LifecycleJsonPredicate::Equals {
                pointer: "/state".to_owned(),
                value: LifecycleJsonScalar::String {
                    value: value.to_owned(),
                },
            }],
        }
    }

    fn contract(
        operation: CredentialLifecycleOperation,
        mode: &str,
        timeout_secs: u32,
        selection: ProfileSelection,
    ) -> SkillRuntimeContract {
        let hook = LifecycleHook {
            args: vec![
                match operation {
                    CredentialLifecycleOperation::Status => "status",
                    CredentialLifecycleOperation::Login => "login",
                    CredentialLifecycleOperation::Logout => "logout",
                    CredentialLifecycleOperation::Refresh => "refresh",
                }
                .to_owned(),
                mode.to_owned(),
            ],
            interaction: if operation == CredentialLifecycleOperation::Login {
                CliInteraction::Pty
            } else {
                CliInteraction::Batch
            },
            timeout_secs: Some(timeout_secs),
        };
        let mut lifecycle = AuthLifecycle {
            status_observation: Some(LifecycleStatusObservation {
                format: LifecycleStatusOutputFormat::Json,
                rules: vec![
                    status_rule(LifecycleObservedAuthState::Ready, "ready"),
                    status_rule(LifecycleObservedAuthState::Missing, "missing"),
                    status_rule(LifecycleObservedAuthState::Expired, "expired"),
                    status_rule(LifecycleObservedAuthState::Revoked, "revoked"),
                ],
            }),
            ..AuthLifecycle::default()
        };
        match operation {
            CredentialLifecycleOperation::Status => lifecycle.status = Some(hook),
            CredentialLifecycleOperation::Login => {
                lifecycle.status = Some(LifecycleHook {
                    args: vec!["status".to_owned(), "ready".to_owned()],
                    interaction: CliInteraction::Batch,
                    timeout_secs: Some(5),
                });
                lifecycle.login = Some(hook);
            },
            CredentialLifecycleOperation::Logout => lifecycle.logout = Some(hook),
            CredentialLifecycleOperation::Refresh => lifecycle.refresh = Some(hook),
        }
        SkillRuntimeContract {
            schema_version: SkillRuntimeContractVersion::v1(),
            requires: RuntimeRequirements {
                bins: BTreeSet::from(["fake-cli".to_owned()]),
                entrypoint: Default::default(),
                environment: Default::default(),
            },
            runtime: RuntimeProtocol::Cli {
                command_prefix: vec!["mail".to_owned()],
                interaction: CliInteraction::Batch,
                stdin: StdinContract::default(),
                working_directory: WorkingDirectoryContract::default(),
                limits: RuntimeLimits::default(),
            },
            auth: AuthContract {
                kind: AuthKind::CliProfile,
                requirement: AuthRequirement::Required,
                provider: Some("google-workspace".to_owned()),
                profile_selection: selection,
                storage: AuthStorage::ScopedDirectory {
                    namespace: "gws".to_owned(),
                    partition_by_profile: true,
                },
                injections: vec![InjectionBinding {
                    source: InjectionSource::ProfileAuthRoot { path: Vec::new() },
                    target: InjectionTarget::Environment {
                        name: "GWS_CONFIG_DIR".to_owned(),
                    },
                }],
                lifecycle,
                identity: IdentityContract::ProfileExpected {
                    selector: IdentitySelector::JsonPointerAsciiCaseInsensitive {
                        pointer: "/account/email".to_owned(),
                    },
                },
                ..AuthContract::default()
            },
            policy_floor: Default::default(),
        }
    }

    fn plan(
        contract: &SkillRuntimeContract,
        profile: CredentialProfileStatus,
        operation: CredentialLifecycleOperation,
    ) -> CredentialLifecyclePlan {
        let validated = validate_skill_runtime_contract(contract).expect("contract");
        CredentialLifecyclePlan::for_profile(
            &TestRegistry {
                profiles: vec![profile.clone()],
            },
            validated,
            profile.key(),
            operation,
        )
        .expect("plan")
    }

    fn fixed_selection() -> ProfileSelection {
        ProfileSelection::Fixed {
            alias: "work".to_owned(),
        }
    }

    struct NoopBridge;

    impl CredentialLifecycleInteractionBridge for NoopBridge {
        fn on_output(
            &mut self,
            _output: CredentialLifecycleSensitiveOutput<'_>,
        ) -> Result<CredentialLifecycleInteractionAction, CredentialLifecycleExecutionBindingError>
        {
            Ok(CredentialLifecycleInteractionAction::Continue)
        }
    }

    struct OtpBridge {
        supplied: bool,
    }

    impl CredentialLifecycleInteractionBridge for OtpBridge {
        fn on_output(
            &mut self,
            output: CredentialLifecycleSensitiveOutput<'_>,
        ) -> Result<CredentialLifecycleInteractionAction, CredentialLifecycleExecutionBindingError>
        {
            if !self.supplied && output.bytes().windows(4).any(|window| window == b"otp-") {
                self.supplied = true;
                return Ok(CredentialLifecycleInteractionAction::ProvideInput {
                    pending: CredentialLifecyclePendingKind::Otp,
                    input: CredentialLifecycleSensitiveInput::new(b"123456\n".to_vec())?,
                });
            }
            Ok(CredentialLifecycleInteractionAction::Continue)
        }
    }

    #[test]
    fn fake_cli_projects_ready_missing_expired_revoked_mismatched_and_malformed() {
        let fixture = Fixture::new();
        let cases = [
            ("ready", AuthState::Ready, false),
            ("missing", AuthState::Missing, false),
            ("expired", AuthState::Expired, false),
            ("revoked", AuthState::Revoked, false),
            ("mismatched", AuthState::IdentityMismatch, false),
            ("malformed", AuthState::Error, true),
        ];
        for (mode, expected, should_fail) in cases {
            let contract = contract(
                CredentialLifecycleOperation::Status,
                mode,
                5,
                fixed_selection(),
            );
            let lifecycle = plan(
                &contract,
                fixture.profile("work", AuthState::Missing),
                CredentialLifecycleOperation::Status,
            );
            let invocation = fixture.invocation(&contract, lifecycle.clone());
            let observed = GovernedCredentialLifecycleExecutor::execute_status_for_plan(
                &lifecycle,
                &invocation,
                &CredentialLifecycleProcessCancellation::new(),
            );
            if should_fail {
                assert_eq!(
                    observed.expect_err("malformed status must fail").code,
                    GovernedLifecycleExecutorErrorCode::ObservationFailed
                );
            } else {
                assert_eq!(observed.expect("status").1.state(), expected, "mode={mode}");
            }
        }
    }

    #[test]
    fn fake_cli_cancellation_and_timeout_are_terminal_and_bounded() {
        let fixture = Fixture::new();
        let cancelled_contract = contract(
            CredentialLifecycleOperation::Status,
            "slow",
            5,
            fixed_selection(),
        );
        let cancelled_plan = plan(
            &cancelled_contract,
            fixture.profile("work", AuthState::Missing),
            CredentialLifecycleOperation::Status,
        );
        let cancelled_invocation = fixture.invocation(&cancelled_contract, cancelled_plan);
        let cancellation = CredentialLifecycleProcessCancellation::new();
        let trigger = cancellation.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(60));
            trigger.cancel();
        });
        let cancelled = GovernedCredentialLifecycleExecutor::execute(
            &cancelled_invocation,
            &cancellation,
            None,
        )
        .expect("cancelled result");
        canceller.join().expect("canceller");
        assert_eq!(
            cancelled.termination(),
            CredentialLifecycleTermination::Cancelled
        );

        let timed_contract = contract(
            CredentialLifecycleOperation::Status,
            "slow",
            1,
            fixed_selection(),
        );
        let timed_plan = plan(
            &timed_contract,
            fixture.profile("work", AuthState::Missing),
            CredentialLifecycleOperation::Status,
        );
        let timed_invocation = fixture.invocation(&timed_contract, timed_plan);
        let timed = GovernedCredentialLifecycleExecutor::execute(
            &timed_invocation,
            &CredentialLifecycleProcessCancellation::new(),
            None,
        )
        .expect("timed result");
        assert_eq!(
            timed.termination(),
            CredentialLifecycleTermination::TimedOut
        );
    }

    #[test]
    fn fake_cli_output_limit_stops_the_process_and_joins_stream_readers() {
        let fixture = Fixture::new();
        let contract = contract(
            CredentialLifecycleOperation::Status,
            "flood",
            5,
            fixed_selection(),
        );
        let lifecycle = plan(
            &contract,
            fixture.profile("work", AuthState::Missing),
            CredentialLifecycleOperation::Status,
        );
        let invocation = fixture.invocation(&contract, lifecycle);
        let error = GovernedCredentialLifecycleExecutor::execute(
            &invocation,
            &CredentialLifecycleProcessCancellation::new(),
            None,
        )
        .expect_err("bounded output must fail closed");
        assert_eq!(
            error.code,
            GovernedLifecycleExecutorErrorCode::OutputLimitExceeded
        );
    }

    #[test]
    fn reader_cleanup_is_bounded_when_an_escaped_writer_keeps_the_pipe_open() {
        let mut descriptors = [0; 2];
        // SAFETY: `pipe` initializes two owned descriptors on success.
        assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
        // SAFETY: each successful `pipe` descriptor is transferred to exactly one File.
        let reader = unsafe { fs::File::from_raw_fd(descriptors[0]) };
        // SAFETY: same ownership transfer for the write end, deliberately retained.
        let writer = unsafe { fs::File::from_raw_fd(descriptors[1]) };
        let (sender, receiver) = mpsc::sync_channel(PROCESS_STREAM_CHANNEL_DEPTH);
        let handle = spawn_stream_reader(
            reader,
            Some(descriptors[0]),
            CredentialLifecycleOutputChannel::Stdout,
            sender,
        );
        let started = Instant::now();
        let result = finish_readers([handle], &receiver, &mut Vec::new(), &mut Vec::new());
        drop(writer);

        assert!(
            result.is_ok(),
            "cooperative reader shutdown should be clean"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "reader cleanup must have a hard deadline"
        );
    }

    #[test]
    fn fake_cli_otp_bridge_and_login_coordinator_complete_only_after_reap() {
        let fixture = Fixture::new();
        let contract = contract(
            CredentialLifecycleOperation::Login,
            "otp",
            5,
            fixed_selection(),
        );
        let lifecycle = plan(
            &contract,
            fixture.profile("work", AuthState::Missing),
            CredentialLifecycleOperation::Login,
        );
        let invocation = fixture.invocation(&contract, lifecycle.clone());
        let epoch =
            CredentialProcessEpoch::new(NEXT_EPOCH.fetch_add(1, Ordering::Relaxed)).expect("epoch");
        let cache = CredentialVerifiedStatusCache::new(
            epoch,
            CredentialPolicyRevision::new(1).expect("policy"),
        );
        let coordinator = CredentialLifecycleCoordinator::new(
            epoch,
            CredentialLifecycleIdleTimeout::new(Duration::from_secs(30)).expect("idle"),
        )
        .expect("coordinator");
        let lease = coordinator
            .begin_login(
                &cache,
                lifecycle,
                CredentialLifecycleCoordinatorInstant::now(),
            )
            .expect("lease");
        let mut bridge = OtpBridge { supplied: false };
        let (result, completion) = GovernedCredentialLifecycleExecutor::execute_login(
            &invocation,
            &coordinator,
            lease,
            &CredentialLifecycleProcessCancellation::new(),
            &mut bridge,
        )
        .expect("login");
        assert_eq!(
            result.termination(),
            CredentialLifecycleTermination::Exited { code: 0 }
        );
        assert_eq!(
            result.last_pending(),
            Some(CredentialLifecyclePendingKind::Otp)
        );
        assert_eq!(
            completion.postcondition,
            Some(
                tool_runtime_core::credential_lifecycle_observation::CredentialLifecycleSuccessPostcondition::FreshStatusRequired
            )
        );
        assert_eq!(coordinator.active_count().expect("count"), 0);
    }

    #[test]
    fn concurrent_login_is_rejected_and_success_still_requires_fresh_status() {
        let fixture = Fixture::new();
        let login_contract = contract(
            CredentialLifecycleOperation::Login,
            "success",
            5,
            fixed_selection(),
        );
        let login_plan = plan(
            &login_contract,
            fixture.profile("work", AuthState::Missing),
            CredentialLifecycleOperation::Login,
        );
        let login_invocation = fixture.invocation(&login_contract, login_plan.clone());
        let epoch =
            CredentialProcessEpoch::new(NEXT_EPOCH.fetch_add(1, Ordering::Relaxed)).expect("epoch");
        let cache = CredentialVerifiedStatusCache::new(
            epoch,
            CredentialPolicyRevision::new(1).expect("policy"),
        );
        let coordinator = CredentialLifecycleCoordinator::new(
            epoch,
            CredentialLifecycleIdleTimeout::new(Duration::from_secs(30)).expect("idle"),
        )
        .expect("coordinator");
        let lease = coordinator
            .begin_login(
                &cache,
                login_plan.clone(),
                CredentialLifecycleCoordinatorInstant::now(),
            )
            .expect("first login");
        let concurrent = coordinator
            .begin_login(
                &cache,
                login_plan,
                CredentialLifecycleCoordinatorInstant::now(),
            )
            .expect_err("concurrent login");
        assert_eq!(
            concurrent.code,
            CredentialLifecycleCoordinatorErrorCode::LeaseAlreadyActive
        );
        let mut bridge = NoopBridge;
        let (_, completion) = GovernedCredentialLifecycleExecutor::execute_login(
            &login_invocation,
            &coordinator,
            lease,
            &CredentialLifecycleProcessCancellation::new(),
            &mut bridge,
        )
        .expect("login");
        assert_eq!(
            completion.postcondition,
            Some(
                tool_runtime_core::credential_lifecycle_observation::CredentialLifecycleSuccessPostcondition::FreshStatusRequired
            )
        );

        let status_contract = contract(
            CredentialLifecycleOperation::Status,
            "ready",
            5,
            fixed_selection(),
        );
        let status_plan = plan(
            &status_contract,
            fixture.profile("work", AuthState::Ready),
            CredentialLifecycleOperation::Status,
        );
        let status_invocation = fixture.invocation(&status_contract, status_plan.clone());
        let (_, observed) = GovernedCredentialLifecycleExecutor::execute_status_for_plan(
            &status_plan,
            &status_invocation,
            &CredentialLifecycleProcessCancellation::new(),
        )
        .expect("fresh status");
        assert!(observed.is_execution_ready());
    }

    #[test]
    fn fixed_profile_cannot_be_replaced_before_process_binding() {
        let fixture = Fixture::new();
        let contract = contract(
            CredentialLifecycleOperation::Status,
            "ready",
            5,
            fixed_selection(),
        );
        let validated = validate_skill_runtime_contract(&contract).expect("contract");
        let requested = fixture.profile("personal", AuthState::Missing);
        let error = CredentialLifecyclePlan::for_profile(
            &TestRegistry {
                profiles: vec![requested.clone()],
            },
            validated,
            requested.key(),
            CredentialLifecycleOperation::Status,
        )
        .expect_err("fixed profile replacement");
        assert_eq!(error.code, CredentialLifecycleErrorCode::TargetMismatch);
    }
}
