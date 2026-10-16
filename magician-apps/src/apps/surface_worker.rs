//! Magician-owned killable custom-surface worker.
//!
//! Scripted `surfaces/*.js` runs in a Magician-spawned OS child, not in the
//! Unified UI iframe and not on the server event loop. The child sees only
//! admitted `surfaces/` bytes, talks through a private Unix socket, has no
//! network, and is killed on disable, quarantine, update, revocation, or a
//! CPU/RSS/wall trip. Display stays the no-script `srcdoc` envelope.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use boa_engine::{js_string, Context, JsNativeError, JsResult, JsValue, NativeFunction, Source};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::{
    manifest::AppPackageCandidate,
    surface_assets::{surface_path_is_javascript, surface_path_is_wasm},
};

pub const SURFACE_WORKER_ENV: &str = "MAGICIAN_SURFACE_WORKER";
pub const SURFACE_WORKER_MANIFEST_ENV: &str = "MAGICIAN_SURFACE_WORKER_MANIFEST";
pub const SURFACE_WORKER_FD_ENV: &str = "MAGICIAN_SURFACE_WORKER_FD";
const TEST_HOG_ENV: &str = "MAGICIAN_SURFACE_WORKER_TEST_HOG";
const TEST_CPU_ENV: &str = "MAGICIAN_SURFACE_WORKER_TEST_CPU";
const TEST_NET_ENV: &str = "MAGICIAN_SURFACE_WORKER_TEST_NET";
const TEST_CHILD_FILTER: &str = "magician_surface_worker_child";

const DEFAULT_MAX_RSS_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_MAX_CPU: Duration = Duration::from_secs(10);
const DEFAULT_MAX_WALL: Duration = Duration::from_secs(15 * 60);
const DEFAULT_POLL: Duration = Duration::from_millis(200);
const MAX_WORKER_EVENT_BYTES: usize = 1024 * 1024;
#[cfg(test)]
const READY_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppSurfaceWorkerBudget {
    pub max_rss_bytes: u64,
    pub max_cpu: Duration,
    pub max_wall: Duration,
    pub poll_interval: Duration,
}

impl Default for AppSurfaceWorkerBudget {
    fn default() -> Self {
        Self {
            max_rss_bytes: DEFAULT_MAX_RSS_BYTES,
            max_cpu: DEFAULT_MAX_CPU,
            max_wall: DEFAULT_MAX_WALL,
            poll_interval: DEFAULT_POLL,
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppSurfaceWorkerError {
    #[error("custom-surface worker could not be spawned: {0}")]
    Spawn(String),
    #[error("custom-surface worker requires OS network isolation")]
    NetworkIsolationUnavailable,
    #[error("custom-surface worker could not seal admitted surfaces")]
    SealedWorkdir,
    #[error("custom-surface worker manifest is invalid")]
    Manifest,
    #[error("custom-surface worker protocol is invalid")]
    Protocol,
    #[error("custom-surface worker exceeded its {0} budget")]
    Budget(&'static str),
    #[error("custom-surface worker javascript failed: {0}")]
    Javascript(String),
    #[error("custom-surface wasm is refused until a wasm worker exists")]
    WasmRefused,
    #[error("custom-surface worker entry script is missing")]
    EntryMissing,
    #[error("custom-surface worker I/O failed: {0}")]
    Io(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppSurfaceWorkerManifest {
    pub session_ref: String,
    pub installation_id: String,
    pub package_revision_ref: String,
    pub workdir: String,
    pub entry: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_probe: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppSurfaceWorkerEvent {
    pub v: u8,
    pub op: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppSurfaceJsOutcome {
    pub render: Option<String>,
    pub logs: Vec<String>,
    pub bridge: Vec<AppSurfaceQueuedBridge>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppSurfaceQueuedBridge {
    pub request_id: String,
    pub sequence: u64,
    pub method: String,
    pub view_or_action: String,
    pub payload: serde_json::Value,
}

pub struct SealedSurfaceWorkdir {
    path: PathBuf,
}

impl SealedSurfaceWorkdir {
    pub fn create() -> Result<Self, AppSurfaceWorkerError> {
        let path = std::env::temp_dir().join(format!(
            "magician-surface-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        fs::create_dir_all(&path).map_err(|error| AppSurfaceWorkerError::Io(error.to_string()))?;
        restrict_dir(&path)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SealedSurfaceWorkdir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub struct AppSurfaceWorkerProcess {
    child: Child,
    reader: BufReader<Box<dyn io::Read + Send>>,
    writer: Box<dyn io::Write + Send>,
    _workdir: SealedSurfaceWorkdir,
    started_at: Instant,
    budget: AppSurfaceWorkerBudget,
    pub pid: u32,
    pub entry: Option<String>,
    pub last_render: Option<String>,
    pub queued_bridge: Vec<AppSurfaceQueuedBridge>,
    pub last_error: Option<String>,
    killed: bool,
}

impl AppSurfaceWorkerProcess {
    pub fn kill(&mut self) {
        if self.killed {
            return;
        }
        self.killed = true;
        let _ = write_event(
            &mut self.writer,
            &AppSurfaceWorkerEvent {
                v: 1,
                op: "shutdown".to_owned(),
                html: None,
                message: None,
                method: None,
                view: None,
                payload: None,
                request_id: None,
                sequence: None,
                ok: None,
                result: None,
                error_code: None,
                pid: None,
            },
        );
        kill_process_tree(self.pid);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn is_alive(&mut self) -> bool {
        match self.child.try_wait() {
            Ok(None) => true,
            _ => false,
        }
    }

    pub fn poll_budget(&mut self) -> Result<(), AppSurfaceWorkerError> {
        if self.started_at.elapsed() >= self.budget.max_wall {
            self.kill();
            return Err(AppSurfaceWorkerError::Budget("wall"));
        }
        if let Some(rss) = process_rss_bytes(self.pid) {
            if rss > self.budget.max_rss_bytes {
                self.kill();
                return Err(AppSurfaceWorkerError::Budget("rss"));
            }
        }
        if let Some(cpu) = process_cpu_time(self.pid) {
            if cpu >= self.budget.max_cpu {
                self.kill();
                return Err(AppSurfaceWorkerError::Budget("cpu"));
            }
        }
        if !self.is_alive() {
            return Ok(());
        }
        Ok(())
    }

    pub fn drain_events(&mut self) {
        while let Some(event) = read_event_nonblocking(&mut self.reader) {
            apply_event(self, event);
        }
    }

    pub fn take_bridge_requests(&mut self) -> Vec<AppSurfaceQueuedBridge> {
        self.drain_events();
        std::mem::take(&mut self.queued_bridge)
    }

    pub fn complete_bridge(
        &mut self,
        request_id: &str,
        sequence: u64,
        result: Result<serde_json::Value, (&str, &str)>,
    ) -> Result<(), AppSurfaceWorkerError> {
        let (ok, value, error_code, message) = match result {
            Ok(value) => (Some(true), Some(value), None, None),
            Err((code, message)) => (
                Some(false),
                None,
                Some(code.to_owned()),
                Some(message.to_owned()),
            ),
        };
        write_event(
            &mut self.writer,
            &AppSurfaceWorkerEvent {
                v: 1,
                op: "bridge_response".to_owned(),
                html: None,
                message,
                method: None,
                view: None,
                payload: None,
                request_id: Some(request_id.to_owned()),
                sequence: Some(sequence),
                ok,
                result: value,
                error_code,
                pid: None,
            },
        )
    }

    pub fn wait_ready(&mut self, timeout: Duration) -> Result<(), AppSurfaceWorkerError> {
        let deadline = Instant::now() + timeout;
        loop {
            if Instant::now() >= deadline {
                return Err(AppSurfaceWorkerError::Spawn(
                    "worker did not become ready".to_owned(),
                ));
            }
            if let Some(event) = read_event_nonblocking(&mut self.reader) {
                let ready = event.op == "ready";
                apply_event(self, event);
                if self.last_error.is_some() {
                    return Err(AppSurfaceWorkerError::Protocol);
                }
                if ready {
                    return Ok(());
                }
            } else {
                thread::sleep(Duration::from_millis(20));
            }
            if !self.is_alive() {
                return Err(AppSurfaceWorkerError::Spawn(
                    "worker exited before ready".to_owned(),
                ));
            }
        }
    }
}

impl Drop for AppSurfaceWorkerProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

pub fn package_has_javascript(candidate: &AppPackageCandidate) -> bool {
    candidate
        .members()
        .iter()
        .any(|member| surface_path_is_javascript(member.path().as_str()))
}

pub fn package_has_wasm(candidate: &AppPackageCandidate) -> bool {
    candidate
        .members()
        .iter()
        .any(|member| surface_path_is_wasm(member.path().as_str()))
}

pub fn javascript_entry(candidate: &AppPackageCandidate) -> Option<String> {
    let mut first = None;
    for member in candidate.members() {
        let path = member.path().as_str();
        if !surface_path_is_javascript(path) {
            continue;
        }
        if path.eq_ignore_ascii_case("surfaces/main.js")
            || path.eq_ignore_ascii_case("surfaces/app.js")
        {
            return Some(path.to_owned());
        }
        if first.is_none() {
            first = Some(path.to_owned());
        }
    }
    first
}

pub fn seal_admitted_surfaces(
    candidate: &AppPackageCandidate,
    dest: &Path,
) -> Result<Vec<String>, AppSurfaceWorkerError> {
    let mut written = Vec::new();
    for member in candidate.members() {
        let relative = member.path().as_str();
        if !relative.starts_with("surfaces/") || relative == "surfaces/" {
            continue;
        }
        if relative.contains('\0') || Path::new(relative).is_absolute() {
            return Err(AppSurfaceWorkerError::SealedWorkdir);
        }
        if surface_path_is_wasm(relative) {
            return Err(AppSurfaceWorkerError::WasmRefused);
        }
        let dest_path = dest.join(relative);
        if !dest_path.starts_with(dest) {
            return Err(AppSurfaceWorkerError::SealedWorkdir);
        }
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| AppSurfaceWorkerError::Io(error.to_string()))?;
        }
        fs::write(&dest_path, member.bytes())
            .map_err(|error| AppSurfaceWorkerError::Io(error.to_string()))?;
        restrict_file(&dest_path)?;
        written.push(relative.to_owned());
    }
    Ok(written)
}

pub fn worker_stripped_env(manifest_path: &Path) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert(SURFACE_WORKER_ENV.to_owned(), "1".to_owned());
    env.insert(
        SURFACE_WORKER_MANIFEST_ENV.to_owned(),
        manifest_path.display().to_string(),
    );
    env
}

pub fn spawn_session_worker(
    candidate: &AppPackageCandidate,
    session_ref: &str,
    installation_id: &str,
    package_revision_ref: &str,
    budget: AppSurfaceWorkerBudget,
) -> Result<AppSurfaceWorkerProcess, AppSurfaceWorkerError> {
    if package_has_wasm(candidate) {
        return Err(AppSurfaceWorkerError::WasmRefused);
    }
    let workdir = SealedSurfaceWorkdir::create()?;
    seal_admitted_surfaces(candidate, workdir.path())?;
    let entry = javascript_entry(candidate);
    if entry.is_none() {
        return Err(AppSurfaceWorkerError::EntryMissing);
    }
    let manifest = AppSurfaceWorkerManifest {
        session_ref: session_ref.to_owned(),
        installation_id: installation_id.to_owned(),
        package_revision_ref: package_revision_ref.to_owned(),
        workdir: workdir.path().display().to_string(),
        entry: entry.clone(),
        test_probe: None,
    };
    let manifest_path = workdir.path().join(".magician-worker.json");
    fs::write(
        &manifest_path,
        serde_json::to_vec(&manifest).map_err(|_| AppSurfaceWorkerError::Manifest)?,
    )
    .map_err(|error| AppSurfaceWorkerError::Io(error.to_string()))?;
    restrict_file(&manifest_path)?;
    spawn_isolated_current_exe(workdir, manifest_path, entry, budget, None, true)
}

pub fn maybe_run_surface_worker() -> bool {
    if std::env::var_os(SURFACE_WORKER_ENV).is_none() {
        return false;
    }
    match run_surface_worker_from_env() {
        Ok(0) => true,
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("magician surface worker: {error}");
            std::process::exit(2);
        },
    }
}

fn requested_test_probe() -> Option<String> {
    if let Some(kind) = std::env::var_os(TEST_HOG_ENV).map(|_| "hog".to_owned()) {
        return Some(kind);
    }
    if std::env::var_os(TEST_CPU_ENV).is_some() {
        return Some("cpu".to_owned());
    }
    if std::env::var_os(TEST_NET_ENV).is_some() {
        return Some("net".to_owned());
    }
    let Ok(path) = std::env::var(SURFACE_WORKER_MANIFEST_ENV) else {
        return None;
    };
    let Ok(bytes) = fs::read(path) else {
        return None;
    };
    serde_json::from_slice::<AppSurfaceWorkerManifest>(&bytes)
        .ok()
        .and_then(|manifest| manifest.test_probe)
}

pub fn run_test_child_if_requested() {
    match requested_test_probe().as_deref() {
        Some("hog") => {
            let mut hog = vec![0u8; 96 * 1024 * 1024];
            for page in hog.chunks_mut(4096) {
                page[0] = 1;
            }
            std::hint::black_box(&hog);
            thread::sleep(Duration::from_secs(60));
            std::process::exit(0);
        },
        Some("cpu") => {
            let mut x = 0u64;
            loop {
                x = x.wrapping_add(1);
                std::hint::black_box(x);
            }
        },
        Some("net") => {
            let ok = std::net::TcpStream::connect_timeout(
                &"1.1.1.1:80".parse().expect("net probe address"),
                Duration::from_secs(2),
            )
            .is_ok();
            if let Ok(manifest_path) = std::env::var(SURFACE_WORKER_MANIFEST_ENV) {
                if let Some(dir) = Path::new(&manifest_path).parent() {
                    let _ = fs::write(
                        dir.join("net.json"),
                        serde_json::json!({ "ok": ok }).to_string(),
                    );
                }
            }
            std::process::exit(0);
        },
        _ => {},
    }
    if std::env::var_os(SURFACE_WORKER_ENV).is_none() {
        return;
    }
    match run_surface_worker_from_env() {
        Ok(code) => std::process::exit(code),
        Err(_) => std::process::exit(2),
    }
}

pub fn run_surface_worker_from_env() -> Result<i32, AppSurfaceWorkerError> {
    let manifest_path =
        std::env::var(SURFACE_WORKER_MANIFEST_ENV).map_err(|_| AppSurfaceWorkerError::Manifest)?;
    let bytes =
        fs::read(&manifest_path).map_err(|error| AppSurfaceWorkerError::Io(error.to_string()))?;
    let manifest: AppSurfaceWorkerManifest =
        serde_json::from_slice(&bytes).map_err(|_| AppSurfaceWorkerError::Manifest)?;
    let workdir = PathBuf::from(&manifest.workdir);
    std::env::set_current_dir(&workdir)
        .map_err(|error| AppSurfaceWorkerError::Io(error.to_string()))?;
    let (reader, mut writer) = worker_stdio()?;
    write_event(
        &mut writer,
        &AppSurfaceWorkerEvent {
            v: 1,
            op: "ready".to_owned(),
            html: None,
            message: None,
            method: None,
            view: None,
            payload: None,
            request_id: None,
            sequence: None,
            ok: None,
            result: None,
            error_code: None,
            pid: Some(std::process::id()),
        },
    )?;
    if let Some(entry) = &manifest.entry {
        let source_path = workdir.join(entry);
        if !source_path.starts_with(&workdir) {
            return Err(AppSurfaceWorkerError::SealedWorkdir);
        }
        let source =
            fs::read_to_string(&source_path).map_err(|_| AppSurfaceWorkerError::EntryMissing)?;
        install_js_bridge_io(reader, writer);
        let evaluated = execute_surface_javascript_inner(&source);
        let (next_reader, mut next_writer) =
            take_js_bridge_io().ok_or(AppSurfaceWorkerError::Protocol)?;
        let outcome = match evaluated {
            Ok(outcome) => outcome,
            Err(error) => {
                let _ = write_event(
                    &mut *next_writer,
                    &AppSurfaceWorkerEvent {
                        v: 1,
                        op: "error".to_owned(),
                        html: None,
                        message: Some("The custom-surface worker failed safely.".to_owned()),
                        method: None,
                        view: None,
                        payload: None,
                        request_id: None,
                        sequence: None,
                        ok: Some(false),
                        result: None,
                        error_code: Some("javascript_failed".to_owned()),
                        pid: None,
                    },
                );
                return Err(error);
            },
        };
        if let Some(html) = &outcome.render {
            write_event(
                &mut *next_writer,
                &AppSurfaceWorkerEvent {
                    v: 1,
                    op: "render".to_owned(),
                    html: Some(html.clone()),
                    message: None,
                    method: None,
                    view: None,
                    payload: None,
                    request_id: None,
                    sequence: None,
                    ok: None,
                    result: None,
                    error_code: None,
                    pid: None,
                },
            )?;
        }
        for log in &outcome.logs {
            write_event(
                &mut *next_writer,
                &AppSurfaceWorkerEvent {
                    v: 1,
                    op: "log".to_owned(),
                    html: None,
                    message: Some(log.clone()),
                    method: None,
                    view: None,
                    payload: None,
                    request_id: None,
                    sequence: None,
                    ok: None,
                    result: None,
                    error_code: None,
                    pid: None,
                },
            )?;
        }
        // Offline unit execution retains queued calls for inspection. A live
        // worker sends each call synchronously from `js_bridge`, so this list
        // must be empty and can never replay after evaluation.
        for request in &outcome.bridge {
            write_event(
                &mut *next_writer,
                &AppSurfaceWorkerEvent {
                    v: 1,
                    op: "bridge".to_owned(),
                    html: None,
                    message: None,
                    method: Some(request.method.clone()),
                    view: Some(request.view_or_action.clone()),
                    payload: Some(request.payload.clone()),
                    request_id: Some(request.request_id.clone()),
                    sequence: Some(request.sequence),
                    ok: None,
                    result: None,
                    error_code: None,
                    pid: None,
                },
            )?;
        }
        return wait_for_worker_shutdown(next_reader);
    }
    wait_for_worker_shutdown(reader)
}

fn wait_for_worker_shutdown(reader: Box<dyn BufRead>) -> Result<i32, AppSurfaceWorkerError> {
    let mut lines = reader.lines();
    while let Some(Ok(line)) = lines.next() {
        if line.len() > MAX_WORKER_EVENT_BYTES {
            return Err(AppSurfaceWorkerError::Protocol);
        }
        let event: AppSurfaceWorkerEvent =
            serde_json::from_str(&line).map_err(|_| AppSurfaceWorkerError::Protocol)?;
        if event.op == "shutdown" {
            break;
        }
    }
    Ok(0)
}

pub fn execute_surface_javascript(
    source: &str,
) -> Result<AppSurfaceJsOutcome, AppSurfaceWorkerError> {
    clear_js_bridge_io();
    execute_surface_javascript_inner(source)
}

fn execute_surface_javascript_inner(
    source: &str,
) -> Result<AppSurfaceJsOutcome, AppSurfaceWorkerError> {
    reset_js_host_outcome();
    let mut context = Context::default();
    context
        .register_global_callable(
            js_string!("__magician_render"),
            1,
            NativeFunction::from_fn_ptr(js_render),
        )
        .map_err(|error| AppSurfaceWorkerError::Javascript(error.to_string()))?;
    context
        .register_global_callable(
            js_string!("__magician_log"),
            1,
            NativeFunction::from_fn_ptr(js_log),
        )
        .map_err(|error| AppSurfaceWorkerError::Javascript(error.to_string()))?;
    context
        .register_global_callable(
            js_string!("__magician_bridge"),
            3,
            NativeFunction::from_fn_ptr(js_bridge),
        )
        .map_err(|error| AppSurfaceWorkerError::Javascript(error.to_string()))?;
    let prelude = r#"
        const magician = Object.freeze({
            render(html) { __magician_render(String(html)); },
            log(message) { __magician_log(String(message)); },
            query(view, payload) { return __magician_bridge("query", String(view), payload); },
            mutate(view, payload) { return __magician_bridge("mutate", String(view), payload); },
            invoke(action, payload) { return __magician_bridge("invoke", String(action), payload); },
            getRun(action, runRef) {
                return __magician_bridge("get_run", String(action), Object.freeze({
                    run_ref: String(runRef)
                }));
            },
            waitRun(action, runRef, maxPolls = 8, pollIntervalMs = 50) {
                return __magician_bridge("wait_run", String(action), Object.freeze({
                    run_ref: String(runRef),
                    max_polls: maxPolls,
                    poll_interval_ms: pollIntervalMs
                }));
            },
            cancelRun(action, runRef, expectedGeneration, idempotencyKey) {
                return __magician_bridge("cancel_run", String(action), Object.freeze({
                    run_ref: String(runRef),
                    expected_generation: expectedGeneration,
                    idempotency_key: String(idempotencyKey)
                }));
            },
            subscribe(afterChangeSequence, limit = 64) {
                return __magician_bridge("subscribe", "changes", Object.freeze({
                    after_change_sequence: afterChangeSequence,
                    limit
                }));
            },
        });
    "#;
    context
        .eval(Source::from_bytes(prelude.as_bytes()))
        .map_err(|error| AppSurfaceWorkerError::Javascript(error.to_string()))?;
    context
        .eval(Source::from_bytes(source.as_bytes()))
        .map_err(|error| AppSurfaceWorkerError::Javascript(error.to_string()))?;
    Ok(JS_HOST.with(|slot| slot.borrow().outcome.clone()))
}

pub fn enforce_worker_budget(
    process: &mut AppSurfaceWorkerProcess,
) -> Result<(), AppSurfaceWorkerError> {
    process.poll_budget()
}

fn spawn_isolated_current_exe(
    workdir: SealedSurfaceWorkdir,
    manifest_path: PathBuf,
    entry: Option<String>,
    budget: AppSurfaceWorkerBudget,
    extra_env: Option<BTreeMap<String, String>>,
    isolate_network: bool,
) -> Result<AppSurfaceWorkerProcess, AppSurfaceWorkerError> {
    spawn_isolated_program(
        current_exe()?,
        workdir,
        manifest_path,
        entry,
        budget,
        extra_env,
        cfg!(test),
        isolate_network,
    )
}

fn spawn_isolated_program(
    exe: PathBuf,
    workdir: SealedSurfaceWorkdir,
    manifest_path: PathBuf,
    entry: Option<String>,
    budget: AppSurfaceWorkerBudget,
    extra_env: Option<BTreeMap<String, String>>,
    test_helper: bool,
    isolate_network: bool,
) -> Result<AppSurfaceWorkerProcess, AppSurfaceWorkerError> {
    #[cfg(unix)]
    {
        use std::os::{
            fd::{FromRawFd, IntoRawFd, OwnedFd},
            unix::net::UnixStream,
        };

        let (parent, child) =
            UnixStream::pair().map_err(|error| AppSurfaceWorkerError::Spawn(error.to_string()))?;
        parent
            .set_read_timeout(Some(Duration::from_millis(50)))
            .map_err(|error| AppSurfaceWorkerError::Spawn(error.to_string()))?;
        let child_fd = clear_cloexec(child.into_raw_fd())?;
        let mut command = isolated_command(&exe, workdir.path(), isolate_network)?;
        command
            .env_clear()
            .env(SURFACE_WORKER_ENV, "1")
            .env(SURFACE_WORKER_MANIFEST_ENV, &manifest_path)
            .env(SURFACE_WORKER_FD_ENV, child_fd.to_string())
            .env("HOME", workdir.path())
            .env("TMPDIR", workdir.path())
            .current_dir(workdir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if test_helper {
            command
                .arg(TEST_CHILD_FILTER)
                .arg("--exact")
                .arg("--nocapture");
        }
        if let Some(extra) = extra_env {
            for (key, value) in extra {
                command.env(key, value);
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child_process = command
            .spawn()
            .map_err(|error| AppSurfaceWorkerError::Spawn(error.to_string()))?;
        // OwnedFd closes the inherited copy in the parent after spawn.
        drop(unsafe { OwnedFd::from_raw_fd(child_fd) });
        let pid = child_process.id();
        let reader = BufReader::new(Box::new(
            parent
                .try_clone()
                .map_err(|error| AppSurfaceWorkerError::Spawn(error.to_string()))?,
        ) as Box<dyn io::Read + Send>);
        let writer = Box::new(parent) as Box<dyn io::Write + Send>;
        Ok(AppSurfaceWorkerProcess {
            child: child_process,
            reader,
            writer,
            _workdir: workdir,
            started_at: Instant::now(),
            budget,
            pid,
            entry,
            last_render: None,
            queued_bridge: Vec::new(),
            last_error: None,
            killed: false,
        })
    }
    #[cfg(not(unix))]
    {
        let _ = (
            exe,
            workdir,
            manifest_path,
            entry,
            budget,
            extra_env,
            test_helper,
            isolate_network,
        );
        Err(AppSurfaceWorkerError::NetworkIsolationUnavailable)
    }
}

fn isolated_command(
    exe: &Path,
    workdir: &Path,
    isolate_network: bool,
) -> Result<Command, AppSurfaceWorkerError> {
    if !isolate_network {
        return Ok(Command::new(exe));
    }
    #[cfg(target_os = "macos")]
    {
        if !Path::new("/usr/bin/sandbox-exec").exists() {
            return Err(AppSurfaceWorkerError::NetworkIsolationUnavailable);
        }
        let mut command = Command::new("/usr/bin/sandbox-exec");
        command
            .arg("-p")
            .arg(macos_worker_sandbox_profile(workdir)?)
            .arg(exe);
        Ok(command)
    }
    #[cfg(target_os = "linux")]
    {
        let _ = workdir;
        if !Path::new("/usr/bin/unshare").exists() {
            return Err(AppSurfaceWorkerError::NetworkIsolationUnavailable);
        }
        let mut command = Command::new("/usr/bin/unshare");
        command
            .args(["--user", "--net", "--map-root-user"])
            .arg(exe);
        Ok(command)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (exe, workdir);
        Err(AppSurfaceWorkerError::NetworkIsolationUnavailable)
    }
}

#[cfg(target_os = "macos")]
fn macos_worker_sandbox_profile(workdir: &Path) -> Result<String, AppSurfaceWorkerError> {
    let path = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());
    let display = path.to_str().ok_or(AppSurfaceWorkerError::SealedWorkdir)?;
    if display
        .chars()
        .any(|ch| matches!(ch, '"' | '(' | ')' | '\\'))
    {
        return Err(AppSurfaceWorkerError::SealedWorkdir);
    }
    // Keep default reads so the Magician binary can load system dylibs.
    // Deny all writes except the sealed workdir. Network stays denied.
    Ok(format!(
        "(version 1)(allow default)(deny network*)(deny file-write*)(allow file-write* (subpath \
         \"{display}\"))"
    ))
}

fn current_exe() -> Result<PathBuf, AppSurfaceWorkerError> {
    std::env::current_exe().map_err(|error| AppSurfaceWorkerError::Spawn(error.to_string()))
}

fn worker_stdio() -> Result<(Box<dyn BufRead>, Box<dyn Write>), AppSurfaceWorkerError> {
    #[cfg(unix)]
    {
        use std::os::{fd::FromRawFd, unix::net::UnixStream};
        if let Ok(raw) = std::env::var(SURFACE_WORKER_FD_ENV) {
            let fd: i32 = raw.parse().map_err(|_| AppSurfaceWorkerError::Protocol)?;
            let stream = unsafe { UnixStream::from_raw_fd(fd) };
            let reader = BufReader::new(
                stream
                    .try_clone()
                    .map_err(|error| AppSurfaceWorkerError::Io(error.to_string()))?,
            );
            return Ok((Box::new(reader), Box::new(stream)));
        }
    }
    Ok((
        Box::new(BufReader::new(io::stdin())),
        Box::new(io::stdout()),
    ))
}

fn write_event(
    writer: &mut dyn Write,
    event: &AppSurfaceWorkerEvent,
) -> Result<(), AppSurfaceWorkerError> {
    let mut line = serde_json::to_string(event).map_err(|_| AppSurfaceWorkerError::Protocol)?;
    if line.len() > MAX_WORKER_EVENT_BYTES {
        return Err(AppSurfaceWorkerError::Protocol);
    }
    line.push('\n');
    writer
        .write_all(line.as_bytes())
        .map_err(|error| AppSurfaceWorkerError::Io(error.to_string()))?;
    writer
        .flush()
        .map_err(|error| AppSurfaceWorkerError::Io(error.to_string()))?;
    Ok(())
}

fn apply_event(process: &mut AppSurfaceWorkerProcess, event: AppSurfaceWorkerEvent) {
    if event.v != 1 {
        process.last_error = Some("worker_protocol".to_owned());
        return;
    }
    match event.op.as_str() {
        "ready" if event.pid.is_some() => {},
        "render" => match event.html {
            Some(html) => process.last_render = Some(html),
            None => process.last_error = Some("worker_protocol".to_owned()),
        },
        "bridge" => {
            if let (Some(request_id), Some(sequence), Some(method), Some(view)) =
                (event.request_id, event.sequence, event.method, event.view)
            {
                if !matches!(
                    method.as_str(),
                    "query"
                        | "mutate"
                        | "invoke"
                        | "subscribe"
                        | "get_run"
                        | "wait_run"
                        | "cancel_run"
                ) {
                    process.last_error = Some("worker_protocol".to_owned());
                    return;
                }
                process.queued_bridge.push(AppSurfaceQueuedBridge {
                    request_id,
                    sequence,
                    method,
                    view_or_action: view,
                    payload: event.payload.unwrap_or(serde_json::Value::Null),
                });
            } else {
                process.last_error = Some("worker_protocol".to_owned());
            }
        },
        "error" => {
            process.last_error = event.error_code.or(Some("worker_failed".to_owned()));
        },
        "log" if event.message.is_some() => {},
        _ => process.last_error = Some("worker_protocol".to_owned()),
    }
}

fn read_event_nonblocking(
    reader: &mut BufReader<Box<dyn io::Read + Send>>,
) -> Option<AppSurfaceWorkerEvent> {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) if line.len() > MAX_WORKER_EVENT_BYTES => Some(protocol_error_event()),
        Ok(_) => Some(serde_json::from_str(line.trim()).unwrap_or_else(|_| protocol_error_event())),
    }
}

fn protocol_error_event() -> AppSurfaceWorkerEvent {
    AppSurfaceWorkerEvent {
        v: 0,
        op: "error".to_owned(),
        html: None,
        message: None,
        method: None,
        view: None,
        payload: None,
        request_id: None,
        sequence: None,
        ok: Some(false),
        result: None,
        error_code: Some("worker_protocol".to_owned()),
        pid: None,
    }
}

fn restrict_dir(path: &Path) -> Result<(), AppSurfaceWorkerError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| AppSurfaceWorkerError::Io(error.to_string()))?;
    }
    let _ = path;
    Ok(())
}

fn restrict_file(path: &Path) -> Result<(), AppSurfaceWorkerError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o400))
            .map_err(|error| AppSurfaceWorkerError::Io(error.to_string()))?;
    }
    let _ = path;
    Ok(())
}

fn kill_process_tree(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::killpg(pid as i32, libc::SIGKILL);
        libc::kill(pid as i32, libc::SIGKILL);
    }
    let _ = pid;
}

fn process_rss_bytes(pid: u32) -> Option<u64> {
    use sysinfo::{Pid, System};
    let mut system = System::new();
    let mut total = 0u64;
    let mut saw = false;
    for target in process_tree_pids(pid) {
        let child = Pid::from_u32(target);
        if !system.refresh_process(child) {
            continue;
        }
        if let Some(process) = system.process(child) {
            total = total.saturating_add(process.memory());
            saw = true;
        }
    }
    saw.then_some(total)
}

fn process_cpu_time(pid: u32) -> Option<Duration> {
    let mut total = Duration::ZERO;
    let mut saw = false;
    for target in process_tree_pids(pid) {
        if let Some(cpu) = process_cpu_time_one(target) {
            total = total.saturating_add(cpu);
            saw = true;
        }
    }
    saw.then_some(total)
}

fn process_tree_pids(root: u32) -> Vec<u32> {
    let mut out = vec![root];
    let mut index = 0;
    while index < out.len() {
        for child in child_pids(out[index]) {
            if !out.contains(&child) {
                out.push(child);
            }
        }
        index += 1;
    }
    out
}

fn child_pids(pid: u32) -> Vec<u32> {
    let output = Command::new("/usr/bin/pgrep")
        .args(["-P", &pid.to_string()])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

fn process_cpu_time_one(pid: u32) -> Option<Duration> {
    #[cfg(target_os = "macos")]
    {
        macos_cpu_time(pid)
    }
    #[cfg(target_os = "linux")]
    {
        linux_cpu_time(pid)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        None
    }
}

#[cfg(target_os = "macos")]
fn macos_cpu_time(pid: u32) -> Option<Duration> {
    macos_ps_cpu_time(pid).or_else(|| {
        let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
        let got = unsafe {
            libc::proc_pidinfo(
                pid as i32,
                libc::PROC_PIDTASKINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        (got > 0).then_some(Duration::from_nanos(
            info.pti_total_user.saturating_add(info.pti_total_system),
        ))
    })
}

#[cfg(target_os = "macos")]
fn macos_ps_cpu_time(pid: u32) -> Option<Duration> {
    let output = Command::new("/bin/ps")
        .args(["-o", "time=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    parse_ps_cputime(std::str::from_utf8(&output.stdout).ok()?)
}

fn parse_ps_cputime(raw: &str) -> Option<Duration> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let mut parts = raw.split(':').rev();
    let seconds = parts.next()?;
    let (seconds, fraction) = seconds
        .split_once('.')
        .map(|(secs, frac)| {
            let micros = frac.get(..6).unwrap_or(frac);
            let padded = format!("{micros:0<6}");
            (secs, padded.parse::<u32>().unwrap_or(0))
        })
        .unwrap_or((seconds, 0));
    let seconds: u64 = seconds.parse().ok()?;
    let minutes: u64 = parts.next().unwrap_or("0").parse().ok()?;
    let hours: u64 = parts.next().unwrap_or("0").parse().ok()?;
    Some(
        Duration::from_secs(hours * 3600 + minutes * 60 + seconds)
            + Duration::from_micros(fraction as u64),
    )
}

#[cfg(target_os = "linux")]
fn linux_cpu_time(pid: u32) -> Option<Duration> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close_paren = stat.rfind(')')?;
    let fields: Vec<&str> = stat[close_paren + 2..].split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks <= 0 {
        return None;
    }
    let ticks = ticks as u64;
    Some(Duration::from_secs_f64(
        (utime.saturating_add(stime)) as f64 / ticks as f64,
    ))
}

#[cfg(unix)]
fn clear_cloexec(fd: i32) -> Result<i32, AppSurfaceWorkerError> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(AppSurfaceWorkerError::Spawn(
            "could not inspect worker socket".to_owned(),
        ));
    }
    let set = unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) };
    if set < 0 {
        return Err(AppSurfaceWorkerError::Spawn(
            "could not inherit worker socket".to_owned(),
        ));
    }
    Ok(fd)
}

type AppSurfaceWorkerReader = Box<dyn BufRead>;
type AppSurfaceWorkerWriter = Box<dyn Write>;

struct AppSurfaceJsHost {
    outcome: AppSurfaceJsOutcome,
    bridge_io: Option<(AppSurfaceWorkerReader, AppSurfaceWorkerWriter)>,
    next_sequence: u64,
}

thread_local! {
    static JS_HOST: std::cell::RefCell<AppSurfaceJsHost> = std::cell::RefCell::new(AppSurfaceJsHost {
        outcome: AppSurfaceJsOutcome {
            render: None,
            logs: Vec::new(),
            bridge: Vec::new(),
        },
        bridge_io: None,
        next_sequence: 0,
    });
}

fn reset_js_host_outcome() {
    JS_HOST.with(|slot| {
        let mut host = slot.borrow_mut();
        host.outcome = AppSurfaceJsOutcome {
            render: None,
            logs: Vec::new(),
            bridge: Vec::new(),
        };
        host.next_sequence = 0;
    });
}

fn install_js_bridge_io(reader: AppSurfaceWorkerReader, writer: AppSurfaceWorkerWriter) {
    JS_HOST.with(|slot| slot.borrow_mut().bridge_io = Some((reader, writer)));
}

fn take_js_bridge_io() -> Option<(AppSurfaceWorkerReader, AppSurfaceWorkerWriter)> {
    JS_HOST.with(|slot| slot.borrow_mut().bridge_io.take())
}

fn clear_js_bridge_io() {
    JS_HOST.with(|slot| slot.borrow_mut().bridge_io = None);
}

fn js_render(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let html = args
        .first()
        .unwrap_or(&JsValue::undefined())
        .to_string(context)?
        .to_std_string_escaped();
    JS_HOST.with(|slot| slot.borrow_mut().outcome.render = Some(html));
    Ok(JsValue::undefined())
}

fn js_log(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let message = args
        .first()
        .unwrap_or(&JsValue::undefined())
        .to_string(context)?
        .to_std_string_escaped();
    JS_HOST.with(|slot| slot.borrow_mut().outcome.logs.push(message));
    Ok(JsValue::undefined())
}

fn js_bridge(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let method = args
        .first()
        .unwrap_or(&JsValue::undefined())
        .to_string(context)?
        .to_std_string_escaped();
    let view = args
        .get(1)
        .unwrap_or(&JsValue::undefined())
        .to_string(context)?
        .to_std_string_escaped();
    let payload = args
        .get(2)
        .cloned()
        .unwrap_or(JsValue::undefined())
        .to_json(context)
        .unwrap_or(serde_json::Value::Null);
    if !matches!(
        method.as_str(),
        "query" | "mutate" | "invoke" | "subscribe" | "get_run" | "wait_run" | "cancel_run"
    ) {
        return Err(JsNativeError::typ()
            .with_message("The custom-surface bridge method is not available.")
            .into());
    }
    JS_HOST.with(|slot| {
        let mut host = slot.borrow_mut();
        host.next_sequence = host.next_sequence.checked_add(1).ok_or_else(|| {
            JsNativeError::range().with_message("The custom-surface bridge sequence was exhausted.")
        })?;
        let sequence = host.next_sequence;
        let request_id = format!("worker-request:{sequence}");
        let request = AppSurfaceQueuedBridge {
            request_id: request_id.clone(),
            sequence,
            method: method.clone(),
            view_or_action: view.clone(),
            payload: payload.clone(),
        };
        let Some((reader, writer)) = host.bridge_io.as_mut() else {
            host.outcome.bridge.push(request);
            return Ok(JsValue::null());
        };
        write_event(
            &mut **writer,
            &AppSurfaceWorkerEvent {
                v: 1,
                op: "bridge".to_owned(),
                html: None,
                message: None,
                method: Some(method),
                view: Some(view),
                payload: Some(payload),
                request_id: Some(request_id.clone()),
                sequence: Some(sequence),
                ok: None,
                result: None,
                error_code: None,
                pid: None,
            },
        )
        .map_err(|_| {
            JsNativeError::error()
                .with_message("The custom-surface bridge could not send its request.")
        })?;
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|_| {
            JsNativeError::error()
                .with_message("The custom-surface bridge response was unavailable.")
        })?;
        if line.is_empty() || line.len() > MAX_WORKER_EVENT_BYTES {
            return Err(JsNativeError::error()
                .with_message("The custom-surface bridge response was invalid.")
                .into());
        }
        let response: AppSurfaceWorkerEvent = serde_json::from_str(line.trim()).map_err(|_| {
            JsNativeError::error().with_message("The custom-surface bridge response was invalid.")
        })?;
        if response.v != 1
            || response.op != "bridge_response"
            || response.request_id.as_deref() != Some(request_id.as_str())
            || response.sequence != Some(sequence)
        {
            return Err(JsNativeError::error()
                .with_message("The custom-surface bridge response did not match its request.")
                .into());
        }
        if response.ok != Some(true) {
            return Err(JsNativeError::error()
                .with_message("The custom-surface bridge request was refused.")
                .into());
        }
        let result = response.result.unwrap_or(serde_json::Value::Null);
        JsValue::from_json(&result, context)
    })
}

pub fn spawn_budget_watchdog(
    workers: Arc<Mutex<Vec<u32>>>,
    stop: Arc<AtomicBool>,
    on_trip: impl Fn(u32, AppSurfaceWorkerError) + Send + 'static,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            thread::sleep(DEFAULT_POLL);
            let pids = workers
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            for pid in pids {
                if let Some(rss) = process_rss_bytes(pid) {
                    if rss > DEFAULT_MAX_RSS_BYTES {
                        on_trip(pid, AppSurfaceWorkerError::Budget("rss"));
                    }
                }
            }
        }
    })
}

#[cfg(test)]
fn probe_name(kind: &str) -> &'static str {
    if kind == TEST_HOG_ENV {
        "hog"
    } else if kind == TEST_CPU_ENV {
        "cpu"
    } else {
        "net"
    }
}

#[cfg(test)]
pub(crate) fn spawn_test_probe(
    kind: &'static str,
    budget: AppSurfaceWorkerBudget,
) -> Result<(AppSurfaceWorkerProcess, PathBuf), AppSurfaceWorkerError> {
    let workdir = SealedSurfaceWorkdir::create()?;
    let workdir_path = workdir.path().to_path_buf();
    let manifest_path = workdir.path().join(".magician-worker.json");
    fs::write(
        &manifest_path,
        serde_json::to_vec(&AppSurfaceWorkerManifest {
            session_ref: "bridge:probe".to_owned(),
            installation_id: "install_1".to_owned(),
            package_revision_ref: "package-revision:reading-list".to_owned(),
            workdir: workdir.path().display().to_string(),
            entry: None,
            test_probe: Some(probe_name(kind).to_owned()),
        })
        .map_err(|_| AppSurfaceWorkerError::Manifest)?,
    )
    .map_err(|error| AppSurfaceWorkerError::Io(error.to_string()))?;
    let mut extra = BTreeMap::new();
    extra.insert(kind.to_owned(), "1".to_owned());
    let isolate_network = kind == TEST_NET_ENV;
    let process = spawn_isolated_current_exe(
        workdir,
        manifest_path,
        None,
        budget,
        Some(extra),
        isolate_network,
    )?;
    Ok((process, workdir_path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use magician::magician_v2::apps::manifest::{
        build_app_package_candidate, tests::valid_skill_document, AppBundleMember, AppPackageLimits,
    };

    fn scripted_candidate(js: &str) -> AppPackageCandidate {
        build_app_package_candidate(
            vec![
                AppBundleMember::regular_file("SKILL.md", valid_skill_document().into_bytes())
                    .unwrap(),
                AppBundleMember::regular_file("workflows/build.md", b"Build a plan.".to_vec())
                    .unwrap(),
                AppBundleMember::regular_file("assets/icon.svg", b"<svg/>".to_vec()).unwrap(),
                AppBundleMember::regular_file(
                    "vendor/skills/summarize/SKILL.md",
                    b"---\nname: summarize\nversion: 2.1.0\n---\n".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file(
                    "vendor/skills/summarize/bin/summarize.py",
                    b"print('summary')\n".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file(
                    "surfaces/index.html",
                    b"<html><body>plan</body></html>".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file("surfaces/app.js", js.as_bytes().to_vec()).unwrap(),
            ],
            &AppPackageLimits::default(),
        )
        .expect("candidate")
    }

    #[test]
    fn parse_ps_cputime_reads_minutes_and_fractional_seconds() {
        assert_eq!(
            parse_ps_cputime("0:01.20"),
            Some(Duration::from_millis(1200))
        );
        assert_eq!(
            parse_ps_cputime("1:02:03.5"),
            Some(Duration::from_secs(3723) + Duration::from_micros(500000))
        );
    }

    #[test]
    fn seal_copies_only_admitted_surfaces() {
        let package = scripted_candidate("magician.render('ok');");
        let workdir = SealedSurfaceWorkdir::create().expect("workdir");
        let written = seal_admitted_surfaces(&package, workdir.path()).expect("seal");
        assert!(written.contains(&"surfaces/app.js".to_owned()));
        assert!(workdir.path().join("surfaces/app.js").exists());
        assert!(!workdir.path().join("SKILL.md").exists());
        assert!(!worker_stripped_env(&workdir.path().join("manifest.json")).contains_key("PATH"));
        assert!(!worker_stripped_env(&workdir.path().join("manifest.json"))
            .keys()
            .any(|key| key.contains("API") || key.contains("MAGICIAN_ROOT")));
    }

    #[test]
    fn wasm_members_are_refused() {
        let package = build_app_package_candidate(
            vec![
                AppBundleMember::regular_file("SKILL.md", valid_skill_document().into_bytes())
                    .unwrap(),
                AppBundleMember::regular_file("workflows/build.md", b"Build a plan.".to_vec())
                    .unwrap(),
                AppBundleMember::regular_file("assets/icon.svg", b"<svg/>".to_vec()).unwrap(),
                AppBundleMember::regular_file(
                    "vendor/skills/summarize/SKILL.md",
                    b"---\nname: summarize\nversion: 2.1.0\n---\n".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file(
                    "vendor/skills/summarize/bin/summarize.py",
                    b"print('summary')\n".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file("surfaces/app.wasm", b"\0asm".to_vec()).unwrap(),
            ],
            &AppPackageLimits::default(),
        )
        .expect("candidate");
        let workdir = SealedSurfaceWorkdir::create().expect("workdir");
        assert_eq!(
            seal_admitted_surfaces(&package, workdir.path()),
            Err(AppSurfaceWorkerError::WasmRefused)
        );
        assert!(package_has_wasm(&package));
    }

    #[test]
    fn javascript_can_render_and_queue_bridge_without_network_apis() {
        let outcome = execute_surface_javascript(
            r#"
                magician.log("boot");
                magician.render("<p>from-worker</p>");
                magician.query("items", { select: ["title"] });
                if (typeof fetch !== "undefined") {
                    throw new Error("fetch must not exist");
                }
            "#,
        )
        .expect("js");
        assert_eq!(outcome.render.as_deref(), Some("<p>from-worker</p>"));
        assert_eq!(outcome.logs, vec!["boot".to_owned()]);
        assert_eq!(outcome.bridge.len(), 1);
        assert_eq!(outcome.bridge[0].request_id, "worker-request:1");
        assert_eq!(outcome.bridge[0].sequence, 1);
        assert_eq!(outcome.bridge[0].method, "query");
        assert_eq!(outcome.bridge[0].view_or_action, "items");
    }

    #[cfg(unix)]
    #[test]
    fn live_worker_bridge_returns_exact_correlated_result_to_javascript() {
        use std::os::unix::net::UnixStream;

        let (host, worker) = UnixStream::pair().expect("socket pair");
        host.set_read_timeout(Some(Duration::from_secs(2)))
            .expect("timeout");
        worker
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("timeout");
        let joined = std::thread::spawn(move || {
            let worker_reader = Box::new(BufReader::new(worker.try_clone().expect("clone")));
            let worker_writer = Box::new(worker);
            install_js_bridge_io(worker_reader, worker_writer);
            let result = execute_surface_javascript_inner(
                r#"
                    const page = magician.query("items", { select: ["title"] });
                    magician.render(page.rows[0].title);
                "#,
            );
            let _ = take_js_bridge_io();
            result
        });

        let mut host_reader = BufReader::new(host.try_clone().expect("clone"));
        let mut request_line = String::new();
        host_reader.read_line(&mut request_line).expect("request");
        let request: AppSurfaceWorkerEvent =
            serde_json::from_str(request_line.trim()).expect("request event");
        assert_eq!(request.op, "bridge");
        assert_eq!(request.request_id.as_deref(), Some("worker-request:1"));
        assert_eq!(request.sequence, Some(1));
        assert_eq!(request.method.as_deref(), Some("query"));
        let mut host_writer = host;
        write_event(
            &mut host_writer,
            &AppSurfaceWorkerEvent {
                v: 1,
                op: "bridge_response".to_owned(),
                html: None,
                message: None,
                method: None,
                view: None,
                payload: None,
                request_id: request.request_id,
                sequence: request.sequence,
                ok: Some(true),
                result: Some(serde_json::json!({"rows": [{"title": "Owned result"}]})),
                error_code: None,
                pid: None,
            },
        )
        .expect("response");
        let outcome = joined.join().expect("worker thread").expect("javascript");
        assert_eq!(outcome.render.as_deref(), Some("Owned result"));
        assert!(outcome.bridge.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn live_worker_bridge_response_loss_fails_closed() {
        use std::os::unix::net::UnixStream;

        let (host, worker) = UnixStream::pair().expect("socket pair");
        worker
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("timeout");
        let joined = std::thread::spawn(move || {
            install_js_bridge_io(
                Box::new(BufReader::new(worker.try_clone().expect("clone"))),
                Box::new(worker),
            );
            let result = execute_surface_javascript_inner(
                r#"magician.getRun("build", "run:app-action:one");"#,
            );
            let _ = take_js_bridge_io();
            result
        });
        let mut host_reader = BufReader::new(host.try_clone().expect("clone"));
        let mut request = String::new();
        host_reader.read_line(&mut request).expect("request");
        assert!(request.contains("get_run"));
        drop(host_reader);
        drop(host);
        assert!(joined.join().expect("worker thread").is_err());
    }

    #[test]
    fn subscribe_is_a_bounded_cursor_request_not_an_inert_method() {
        let outcome = execute_surface_javascript("magician.subscribe(41, 16);").expect("js");
        assert_eq!(outcome.bridge.len(), 1);
        assert_eq!(outcome.bridge[0].method, "subscribe");
        assert_eq!(outcome.bridge[0].view_or_action, "changes");
        assert_eq!(
            outcome.bridge[0].payload,
            serde_json::json!({"after_change_sequence": 41, "limit": 16})
        );
    }

    #[test]
    fn javascript_exposes_only_action_bound_run_poll_and_generation_cancel() {
        let outcome = execute_surface_javascript(
            r#"
                magician.getRun("build", "run:app-action:one");
                magician.waitRun("build", "run:app-action:one", 4, 50);
                magician.cancelRun("build", "run:app-action:one", 0, "cancel:one");
            "#,
        )
        .expect("run controls");
        assert_eq!(outcome.bridge.len(), 3);
        assert_eq!(outcome.bridge[0].method, "get_run");
        assert_eq!(outcome.bridge[0].view_or_action, "build");
        assert_eq!(
            outcome.bridge[0].payload,
            serde_json::json!({"run_ref": "run:app-action:one"})
        );
        assert_eq!(outcome.bridge[1].method, "wait_run");
        assert_eq!(
            outcome.bridge[1].payload,
            serde_json::json!({
                "run_ref": "run:app-action:one",
                "max_polls": 4,
                "poll_interval_ms": 50
            })
        );
        assert_eq!(outcome.bridge[2].method, "cancel_run");
        assert_eq!(
            outcome.bridge[2].payload,
            serde_json::json!({
                "run_ref": "run:app-action:one",
                "expected_generation": 0,
                "idempotency_key": "cancel:one"
            })
        );
    }

    #[test]
    fn invented_network_calls_fail_closed_in_javascript() {
        let error = execute_surface_javascript("fetch('http://127.0.0.1');").expect_err("fetch");
        match error {
            AppSurfaceWorkerError::Javascript(message) => {
                assert!(
                    message.to_ascii_lowercase().contains("fetch")
                        || message.to_ascii_lowercase().contains("undefined"),
                    "{message}"
                );
            },
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn spawned_worker_runs_js_and_dies_on_kill() {
        let package = scripted_candidate("magician.render('<p>live</p>');");
        let mut worker = spawn_session_worker(
            &package,
            "bridge:session-worker",
            "install_1",
            "package-revision:reading-list",
            AppSurfaceWorkerBudget::default(),
        )
        .expect("spawn");
        worker.wait_ready(READY_TIMEOUT).expect("ready");
        let started = Instant::now();
        while worker.last_render.is_none() && started.elapsed() < Duration::from_secs(2) {
            worker.drain_events();
            if !worker.is_alive() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        worker.drain_events();
        assert_eq!(worker.last_render.as_deref(), Some("<p>live</p>"));
        let pid = worker.pid;
        worker.kill();
        thread::sleep(Duration::from_millis(50));
        assert!(!worker.is_alive());
        assert!(process_rss_bytes(pid).is_none() || !worker.is_alive());
    }

    #[test]
    fn rss_budget_kills_the_child() {
        let (mut worker, _) = spawn_test_probe(
            TEST_HOG_ENV,
            AppSurfaceWorkerBudget {
                max_rss_bytes: 32 * 1024 * 1024,
                max_cpu: Duration::from_secs(30),
                max_wall: Duration::from_secs(30),
                poll_interval: Duration::from_millis(50),
            },
        )
        .expect("hog");
        let mut tripped = None;
        for _ in 0..80 {
            thread::sleep(Duration::from_millis(100));
            if let Err(error) = worker.poll_budget() {
                tripped = Some(error);
                break;
            }
        }
        assert_eq!(tripped, Some(AppSurfaceWorkerError::Budget("rss")));
        assert!(!worker.is_alive());
    }

    #[test]
    fn cpu_budget_kills_the_child() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "while :; do :; done"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().expect("cpu hog");
        let pid = child.id();
        let budget = AppSurfaceWorkerBudget {
            max_rss_bytes: 256 * 1024 * 1024,
            max_cpu: Duration::from_millis(400),
            max_wall: Duration::from_secs(30),
            poll_interval: Duration::from_millis(50),
        };
        let started = Instant::now();
        let mut tripped = None;
        while started.elapsed() < Duration::from_secs(8) {
            thread::sleep(Duration::from_millis(100));
            if process_cpu_time(pid).is_some_and(|cpu| cpu >= budget.max_cpu)
                || started.elapsed() >= budget.max_wall
            {
                kill_process_tree(pid);
                let _ = child.kill();
                let _ = child.wait();
                tripped = Some(AppSurfaceWorkerError::Budget("cpu"));
                break;
            }
        }
        if tripped.is_none() {
            kill_process_tree(pid);
            let _ = child.kill();
            let _ = child.wait();
        }
        assert_eq!(tripped, Some(AppSurfaceWorkerError::Budget("cpu")));
        assert!(child.try_wait().ok().flatten().is_some() || process_rss_bytes(pid).is_none());
    }

    #[test]
    fn isolated_child_cannot_open_the_network() {
        let (mut worker, workdir) = spawn_test_probe(
            TEST_NET_ENV,
            AppSurfaceWorkerBudget {
                max_rss_bytes: 256 * 1024 * 1024,
                max_cpu: Duration::from_secs(30),
                max_wall: Duration::from_secs(30),
                poll_interval: Duration::from_millis(50),
            },
        )
        .expect("net");
        let result_path = workdir.join("net.json");
        let mut saw = None;
        for _ in 0..50 {
            thread::sleep(Duration::from_millis(100));
            if let Ok(bytes) = fs::read(&result_path) {
                saw = serde_json::from_slice::<serde_json::Value>(&bytes).ok();
                break;
            }
            let _ = worker.is_alive();
        }
        worker.kill();
        let ok = saw
            .as_ref()
            .and_then(|value| value.get("ok"))
            .and_then(|value| value.as_bool());
        assert_eq!(ok, Some(false), "worker network probe: {saw:?}");
    }
}
