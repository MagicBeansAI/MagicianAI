//! Runtime-only bounded media ownership. The existing task actor admits this
//! lease for its exact work area; model arguments cannot create or widen it.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
};

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::{
    ffmpeg,
    ops::{MediaOp, OpContext},
};
use crate::magician_v2::execution::{
    agent_resources::AgentResources,
    native_executors::{validate_file_read_path, validate_file_write_path},
};

struct Boundary {
    owner_id: String,
    root: PathBuf,
    maximum_output_bytes: u64,
    deadline: DateTime<Utc>,
    cancel: CancellationToken,
    open: AtomicBool,
    executable_hash: String,
    program: MediaProgram,
    active_jobs: Mutex<usize>,
    drained: tokio::sync::Notify,
}

#[derive(Clone, Copy)]
enum MediaProgram {
    Ffmpeg,
    Ffprobe,
}
impl MediaProgram {
    fn pin(self) -> Result<(PathBuf, String)> {
        match self {
            Self::Ffmpeg => ffmpeg::executable_pin(),
            Self::Ffprobe => ffmpeg::probe_executable_pin(),
        }
    }
}

struct MediaJob {
    boundary: Arc<Boundary>,
}
impl Drop for MediaJob {
    fn drop(&mut self) {
        let mut count = self
            .boundary
            .active_jobs
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *count -= 1;
        if *count == 0 {
            self.boundary.drained.notify_waiters();
        }
        let remove = *count == 0 && !self.boundary.open.load(Ordering::Acquire);
        drop(count);
        if remove {
            remove_boundary(&self.boundary);
        }
    }
}
impl Boundary {
    fn enter(self: &Arc<Self>) -> Result<MediaJob> {
        let mut count = self
            .active_jobs
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        ensure!(
            self.open.load(Ordering::Acquire)
                && !self.cancel.is_cancelled()
                && self.deadline > Utc::now(),
            "media owner closed before dispatch"
        );
        *count = count
            .checked_add(1)
            .context("media active job count overflow")?;
        Ok(MediaJob {
            boundary: self.clone(),
        })
    }
}

fn boundaries() -> &'static Mutex<BTreeMap<PathBuf, Arc<Boundary>>> {
    static OWNERS: OnceLock<Mutex<BTreeMap<PathBuf, Arc<Boundary>>>> = OnceLock::new();
    OWNERS.get_or_init(Default::default)
}

fn remove_boundary(boundary: &Arc<Boundary>) {
    let mut owners = boundaries()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if owners
        .get(&boundary.root)
        .is_some_and(|current| Arc::ptr_eq(current, boundary))
    {
        owners.remove(&boundary.root);
    }
}

pub(crate) struct ScopedMediaLease {
    boundary: Arc<Boundary>,
}

impl ScopedMediaLease {
    /// Own the entire evidence preparation, including local provider work and
    /// sealed result publication. Dropping the waiter cancels productive work;
    /// the owned job still drains and can publish a known/unknown receipt.
    pub async fn run_preparation<F, Fut, T>(self, prepare: F) -> Result<T>
    where
        F: FnOnce(Self, CancellationToken) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T>> + Send + 'static,
        T: Send + 'static,
    {
        let job = self.boundary.enter()?;
        let cancel = self.boundary.cancel.clone();
        let _cancel_on_drop = cancel.clone().drop_guard();
        crate::magician_v2::execution::runtime_boundary::spawn_execution_job(move || async move {
            let _job = job;
            prepare(self, cancel).await
        })
        .await
        .context("media preparation owner stopped")?
    }

    pub async fn close_and_drain(&self) {
        self.boundary.open.store(false, Ordering::Release);
        self.boundary.cancel.cancel();
        loop {
            let notified = self.boundary.drained.notified();
            if *self
                .boundary
                .active_jobs
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                == 0
            {
                break;
            }
            notified.await;
        }
        remove_boundary(&self.boundary);
    }

    pub async fn run_owned(
        &self,
        executable: &Path,
        args: Vec<String>,
        keepalive: Arc<tempfile::TempDir>,
        timeout: std::time::Duration,
    ) -> Result<()> {
        run_owned(
            self.boundary.clone(),
            executable,
            args,
            keepalive,
            timeout,
            self.boundary.cancel.child_token(),
        )
        .await
    }
}

impl Drop for ScopedMediaLease {
    fn drop(&mut self) {
        self.boundary.open.store(false, Ordering::Release);
        self.boundary.cancel.cancel();
        if *self
            .boundary
            .active_jobs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            == 0
        {
            remove_boundary(&self.boundary);
        }
    }
}

pub(crate) fn admit(
    owner_id: &str,
    root: &Path,
    maximum_output_bytes: u64,
    deadline: DateTime<Utc>,
    cancel: CancellationToken,
    executable_hash: &str,
) -> Result<ScopedMediaLease> {
    admit_program(
        owner_id,
        root,
        maximum_output_bytes,
        deadline,
        cancel,
        executable_hash,
        MediaProgram::Ffmpeg,
    )
}

pub(crate) fn admit_probe(
    owner_id: &str,
    root: &Path,
    maximum_output_bytes: u64,
    deadline: DateTime<Utc>,
    cancel: CancellationToken,
    executable_hash: &str,
) -> Result<ScopedMediaLease> {
    admit_program(
        owner_id,
        root,
        maximum_output_bytes,
        deadline,
        cancel,
        executable_hash,
        MediaProgram::Ffprobe,
    )
}

fn admit_program(
    owner_id: &str,
    root: &Path,
    maximum_output_bytes: u64,
    deadline: DateTime<Utc>,
    cancel: CancellationToken,
    executable_hash: &str,
    program: MediaProgram,
) -> Result<ScopedMediaLease> {
    let root = root.canonicalize()?;
    ensure!(
        !owner_id.is_empty()
            && root.is_dir()
            && maximum_output_bytes > 0
            && deadline > Utc::now()
            && !cancel.is_cancelled(),
        "invalid scoped media admission"
    );
    ensure!(
        program.pin()?.1 == executable_hash,
        "media executable changed at admission"
    );
    let boundary = Arc::new(Boundary {
        owner_id: owner_id.into(),
        root: root.clone(),
        maximum_output_bytes,
        deadline,
        cancel: cancel.child_token(),
        open: AtomicBool::new(true),
        executable_hash: executable_hash.to_owned(),
        program,
        active_jobs: Mutex::new(0),
        drained: tokio::sync::Notify::new(),
    });
    let mut owners = boundaries()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    ensure!(
        !owners.contains_key(&root),
        "media work area already has an active owner"
    );
    owners.insert(root, boundary.clone());
    Ok(ScopedMediaLease { boundary })
}

pub(crate) async fn quiesce(owner_id: &str) {
    let owners: Vec<_> = boundaries()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .values()
        .filter(|boundary| boundary.owner_id == owner_id)
        .cloned()
        .collect();
    for boundary in owners {
        ScopedMediaLease { boundary }.close_and_drain().await;
    }
}

async fn run_owned(
    boundary: Arc<Boundary>,
    executable: &Path,
    args: Vec<String>,
    keepalive: Arc<tempfile::TempDir>,
    timeout: std::time::Duration,
    cancel: CancellationToken,
) -> Result<()> {
    let (pinned, hash) = boundary.program.pin()?;
    ensure!(
        executable.canonicalize()? == pinned && hash == boundary.executable_hash,
        "media executable changed before owned dispatch"
    );
    let job = boundary.enter()?;
    struct CancelCaller(CancellationToken);
    impl Drop for CancelCaller {
        fn drop(&mut self) {
            self.0.cancel();
        }
    }
    let _caller = CancelCaller(cancel.clone());
    let executable = pinned
        .to_str()
        .context("media executable path is not UTF-8")?
        .to_owned();
    tokio::spawn(async move {
        let _job = job;
        let _keepalive = keepalive;
        let remaining = (boundary.deadline - Utc::now())
            .to_std()
            .context("media deadline expired before spawn")?;
        ensure!(
            boundary.open.load(Ordering::Acquire),
            "media owner closed before spawn"
        );
        let result = match boundary.program {
            MediaProgram::Ffmpeg => {
                ffmpeg::run_ffmpeg_cancellable(
                    &executable,
                    &args,
                    None,
                    cancel,
                    Some(timeout.min(remaining)),
                    |_| {},
                )
                .await
            },
            MediaProgram::Ffprobe => {
                ffmpeg::run_ffprobe_cancellable(
                    &executable,
                    &args,
                    cancel,
                    Some(timeout.min(remaining)),
                )
                .await
            },
        };
        result.map_err(anyhow::Error::msg)
    })
    .await
    .context("media process owner stopped")?
}

pub(crate) fn input_options(media_type: &str) -> Result<Vec<String>> {
    let format = match media_type {
        "video/mp4" => "mov",
        "video/webm" => "matroska",
        "audio/mpeg" => "mp3",
        "audio/wav" => "wav",
        "image/png" => "png_pipe",
        "image/jpeg" => "jpeg_pipe",
        "image/webp" => "webp_pipe",
        "image/x-portable-pixmap" => "ppm_pipe",
        _ => anyhow::bail!("unsupported bounded media input representation"),
    };
    let mut options = vec![
        "-protocol_whitelist".into(),
        "file,pipe".into(),
        "-f".into(),
        format.into(),
    ];
    if format == "mov" {
        options.extend(["-enable_drefs".into(), "0".into()]);
    }
    Ok(options)
}

pub(super) async fn execute(
    resources: &Arc<AgentResources>,
    args: &Value,
    op: &dyn MediaOp,
) -> Result<Value> {
    ensure!(
        magicllm::current_physical_resource_guard().is_some(),
        "bounded media requires its native resource carrier"
    );
    ensure!(
        resources.file_sandbox.mode == runtime_core::FileSandboxMode::WorkspaceWrite
            && resources.file_sandbox.allowed_roots.len() == 1,
        "bounded media requires one exact writable work area"
    );
    let root = PathBuf::from(&resources.file_sandbox.allowed_roots[0]).canonicalize()?;
    let boundary = boundaries()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&root)
        .cloned()
        .context("bounded media has no runtime admission")?;
    ensure!(
        matches!(boundary.program, MediaProgram::Ffmpeg)
            && boundary.open.load(Ordering::Acquire)
            && !boundary.cancel.is_cancelled()
            && boundary.deadline > Utc::now(),
        "bounded media admission expired"
    );
    let _caller_job = boundary.enter()?;
    let raw_inputs = args
        .get("input_paths")
        .and_then(Value::as_array)
        .context("media input paths missing")?;
    let media_types = args
        .get("input_media_types")
        .and_then(Value::as_array)
        .context("bounded media requires declared input_media_types")?;
    ensure!(
        !raw_inputs.is_empty() && raw_inputs.len() == media_types.len(),
        "media input representations differ from paths"
    );
    if op.name() == "still_to_video" {
        ensure!(
            media_types.len() == 1
                && matches!(
                    media_types[0].as_str(),
                    Some("image/png" | "image/jpeg" | "image/webp" | "image/x-portable-pixmap")
                ),
            "still_to_video requires one declared PNG, JPEG, WebP or PPM still"
        );
    }
    let inputs = raw_inputs
        .iter()
        .map(|raw| {
            let raw = raw.as_str().context("media input path is not a string")?;
            let path = if Path::new(raw).is_absolute() {
                PathBuf::from(raw)
            } else {
                root.join(raw)
            };
            validate_file_read_path(&path, &resources.file_sandbox).map_err(anyhow::Error::new)?;
            let canonical = path.canonicalize()?;
            ensure!(
                canonical.starts_with(&root) && canonical.is_file(),
                "media input is outside its admitted work area"
            );
            Ok(canonical)
        })
        .collect::<Result<Vec<_>>>()?;
    let output_name = args
        .get("output_path")
        .and_then(Value::as_str)
        .context("bounded media requires output_path")?;
    let output = if Path::new(output_name).is_absolute() {
        PathBuf::from(output_name)
    } else {
        root.join(output_name)
    };
    validate_file_write_path(&output, &resources.file_sandbox).map_err(anyhow::Error::new)?;
    let params = args.get("params").cloned().unwrap_or_else(|| json!({}));
    super::ops::validate_params(op, &params).map_err(anyhow::Error::new)?;
    ensure!(
        !op.requires_probe(&params),
        "bounded media requires explicit sampling parameters"
    );
    // The child writes only a scratch file. Dropping or cancelling a caller
    // cannot leave it modifying a published candidate after the task settles.
    let scratch = Arc::new(tempfile::tempdir_in(&root)?);
    let staged = scratch
        .path()
        .join(format!("result.{}", op.output_ext(&params)));
    let context = OpContext {
        inputs: &inputs,
        params: &params,
        output_path: &staged,
        probed_duration_secs: None,
    };
    let arguments = op
        .build_bounded_args(&context)
        .map_err(anyhow::Error::new)?;
    let options = media_types
        .iter()
        .map(|media_type| {
            input_options(
                media_type
                    .as_str()
                    .context("input media type is not a string")?,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let mut bounded = Vec::new();
    let mut input_index = 0;
    for argument in arguments {
        if argument == "-i" {
            bounded.extend(
                options
                    .get(input_index)
                    .context("media operation widened its input set")?
                    .clone(),
            );
            input_index += 1;
        }
        if argument == staged.to_string_lossy().as_ref() {
            bounded.extend([
                "-threads".into(),
                "1".into(),
                "-fs".into(),
                boundary.maximum_output_bytes.to_string(),
            ]);
        }
        bounded.push(argument);
    }
    ensure!(
        input_index == inputs.len(),
        "media operation lost its declared input set"
    );
    let remaining = (boundary.deadline - Utc::now())
        .to_std()
        .context("bounded media deadline expired")?;
    let dispatch_cancel = crate::magician_v2::execution::compiled_dispatch::EXECUTION_CANCEL_TOKEN
        .try_with(Clone::clone)
        .ok()
        .flatten();
    let process_cancel = boundary.cancel.child_token();
    let executable = ffmpeg::executable_pin()?.0;
    let run = run_owned(
        boundary.clone(),
        &executable,
        bounded,
        scratch.clone(),
        remaining,
        process_cancel.clone(),
    );
    tokio::pin!(run);
    let outcome = if let Some(dispatch_cancel) = dispatch_cancel {
        tokio::select! {
            biased;
            _ = dispatch_cancel.cancelled() => { process_cancel.cancel(); run.await },
            result = &mut run => result,
        }
    } else {
        run.await
    };
    outcome?;
    ensure!(
        boundary.open.load(Ordering::Acquire)
            && !boundary.cancel.is_cancelled()
            && boundary.deadline > Utc::now(),
        "media owner left before result publication"
    );
    let metadata = std::fs::symlink_metadata(&staged)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "media result must be a regular scratch file"
    );
    let size = metadata.len();
    ensure!(
        size > 0 && size <= boundary.maximum_output_bytes,
        "media result exceeds its admitted byte ceiling"
    );
    validate_file_write_path(&output, &resources.file_sandbox).map_err(anyhow::Error::new)?;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::magician_v2::artifact_v2::io::publish_staged_file_durably_sync(&staged, &output)?;
    Ok(json!({"status":"completed","output_path":output,"bytes":size}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn relentless_owned_probe_writes_real_json_without_ffmpeg_progress_options() {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let input = directory.path().join("input.wav");
        let output = directory.path().join("probe.json");
        // A valid one-second mono PCM WAV, independent of any owner media.
        let mut wav = Vec::new();
        wav.extend(b"RIFF");
        wav.extend(16036u32.to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(8000u32.to_le_bytes());
        wav.extend(16000u32.to_le_bytes());
        wav.extend(2u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(16000u32.to_le_bytes());
        wav.resize(16044, 0);
        std::fs::write(&input, wav).unwrap();
        let (executable, hash) = ffmpeg::probe_executable_pin().unwrap();
        let owner = admit_probe(
            &format!("probe-{}", uuid::Uuid::new_v4()),
            directory.path(),
            4096,
            Utc::now() + chrono::Duration::seconds(10),
            CancellationToken::new(),
            &hash,
        )
        .unwrap();
        let args = [
            "-v",
            "error",
            "-show_entries",
            "format=duration:stream=codec_type",
            "-of",
            "json",
            "-o",
        ]
        .into_iter()
        .map(String::from)
        .chain([output.display().to_string(), input.display().to_string()])
        .collect();
        let result = owner
            .run_owned(
                &executable,
                args,
                directory.clone(),
                std::time::Duration::from_secs(5),
            )
            .await;
        owner.close_and_drain().await;
        result.unwrap();
        let facts: Value = serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
        assert_eq!(facts["streams"][0]["codec_type"], "audio");
        assert!(
            (facts["format"]["duration"]
                .as_str()
                .unwrap()
                .parse::<f64>()
                .unwrap()
                - 1.0)
                .abs()
                < 0.001
        );
    }

    #[tokio::test]
    async fn relentless_disconnected_preparation_cancels_and_publishes_before_owner_drains() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let owner_id = format!("evidence-{}", uuid::Uuid::new_v4());
        // This tests preparation ownership without dispatching ffmpeg or STT.
        let boundary = Arc::new(Boundary {
            owner_id: owner_id.clone(),
            root: root.clone(),
            maximum_output_bytes: 1024,
            deadline: Utc::now() + chrono::Duration::seconds(30),
            cancel: CancellationToken::new(),
            open: AtomicBool::new(true),
            executable_hash: String::new(),
            program: MediaProgram::Ffmpeg,
            active_jobs: Mutex::new(0),
            drained: tokio::sync::Notify::new(),
        });
        boundaries()
            .lock()
            .unwrap()
            .insert(root.clone(), boundary.clone());
        let (started, dispatched) = tokio::sync::oneshot::channel();
        let (published, receipt) = tokio::sync::oneshot::channel();
        let lease = ScopedMediaLease {
            boundary: boundary.clone(),
        };
        let waiter = tokio::spawn(lease.run_preparation(move |lease, cancel| async move {
            let _lease = lease;
            started.send(()).unwrap();
            cancel.cancelled().await;
            published.send("sealed unknown receipt").unwrap();
            Ok(())
        }));
        dispatched.await.unwrap();
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        tokio::time::timeout(std::time::Duration::from_secs(5), quiesce(&owner_id))
            .await
            .unwrap();
        assert_eq!(receipt.await.unwrap(), "sealed unknown receipt");
        assert_eq!(*boundary.active_jobs.lock().unwrap(), 0);
        assert!(!boundaries().lock().unwrap().contains_key(&root));
    }
}
