//! macOS native audio bridge for the meeting bot (the `AudioSource`/`AudioSink`
//! impls).
//!
//! * **Capture** ([`ScreenCaptureAudioSource`]) spawns the
//!   `magician-macos-meet-audio` Swift helper, which taps the target app's audio
//!   via **ScreenCaptureKit** (no virtual device, no Chrome speaker-routing —
//!   the routing the BlackHole spike kept failing on) and streams 16 kHz mono
//!   PCM16 on stdout. We read it into [`AudioChunk`]s for the streaming STT.
//! * **Inject** ([`SoxAudioSink`]) pipes the responder's PCM16 to
//!   `sox -t coreaudio "BlackHole 16ch"` — the proven Spike-0/Test-B path. This
//!   is the interim sink; a native CoreAudio inject (in the same helper) is the
//!   planned follow-up.
//!
//! The helper binary is located via `MAGICIAN_MACOS_MEET_AUDIO_BIN` (default
//! `./magician-macos-meet-audio.bin`, the repo-root staging target of
//! `make build-macos-presence-host-debug`). Capture needs the Screen Recording
//! TCC grant.

use std::process::Stdio;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::io::AsyncWriteExt;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::magician_v2::media_seam::audio::{AudioError, AudioSink, AudioSource, CaptureTarget};
use crate::magician_v2::media_seam::{AudioChunk, StreamAudioFormat};

pub const MEET_AUDIO_HELPER_ENV: &str = "MAGICIAN_MACOS_MEET_AUDIO_BIN";
pub const DEFAULT_MEET_AUDIO_HELPER: &str = "./magician-macos-meet-audio.bin";
pub const DEFAULT_TARGET_BUNDLE_ID: &str = "com.google.Chrome";
pub const DEFAULT_INJECT_DEVICE: &str = "BlackHole 16ch";

/// Read size for the helper's PCM stdout. Chunk boundaries don't matter — the
/// streaming STT re-segments — so this is just a throughput/latency knob.
const READ_BUF_BYTES: usize = 4096;

fn helper_path() -> String {
    std::env::var(MEET_AUDIO_HELPER_ENV).unwrap_or_else(|_| DEFAULT_MEET_AUDIO_HELPER.to_string())
}

/// Captures a macOS app's audio via the ScreenCaptureKit helper.
///
/// The capture [`target`](CaptureTarget) is interior-mutable so a session can
/// late-bind it (e.g. retarget to the just-launched browser's PID) after the
/// source has been constructed and handed off as `Arc<dyn AudioSource>`.
pub struct ScreenCaptureAudioSource {
    helper_path: String,
    target: std::sync::Mutex<CaptureTarget>,
    format: StreamAudioFormat,
}

impl Default for ScreenCaptureAudioSource {
    fn default() -> Self {
        Self {
            helper_path: helper_path(),
            target: std::sync::Mutex::new(CaptureTarget::BundleId(
                DEFAULT_TARGET_BUNDLE_ID.to_string(),
            )),
            format: StreamAudioFormat::default(),
        }
    }
}

impl ScreenCaptureAudioSource {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_bundle_id(mut self, bundle_id: impl Into<String>) -> Self {
        *self.target.get_mut().unwrap() = CaptureTarget::BundleId(bundle_id.into());
        self
    }

    pub fn with_helper_path(mut self, path: impl Into<String>) -> Self {
        self.helper_path = path.into();
        self
    }

    pub fn with_format(mut self, format: StreamAudioFormat) -> Self {
        self.format = format;
        self
    }
}

#[async_trait]
impl AudioSource for ScreenCaptureAudioSource {
    async fn set_capture_target(&self, target: CaptureTarget) {
        *self.target.lock().unwrap() = target;
    }

    async fn run(&self, out: mpsc::Sender<AudioChunk>) -> Result<(), AudioError> {
        // Copy the flag+value out and drop the lock BEFORE any await — the
        // std Mutex guard is not Send and must not cross an await point.
        let (flag, value) = match &*self.target.lock().unwrap() {
            CaptureTarget::Pid(pid) => ("--target-pid", pid.to_string()),
            CaptureTarget::BundleId(b) => ("--target-bundle-id", b.clone()),
            CaptureTarget::DisplayAudio => ("--display-audio", "true".to_string()),
        };
        let mut child = Command::new(&self.helper_path)
            .arg(flag)
            .arg(value)
            .arg("--sample-rate")
            .arg(self.format.sample_rate_hz.to_string())
            .arg("--channels")
            .arg(self.format.channels.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // The session cancels capture by DROPPING this run() future, so the
            // end-of-loop start_kill below never runs on that path — without
            // kill_on_drop a helper whose capture callback has stalled would
            // linger holding the Screen Recording capture.
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| AudioError::Device(format!("spawn {}: {e}", self.helper_path)))?;

        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| AudioError::Device("meet-audio helper produced no stdout".into()))?;

        // Drain the helper's JSON diagnostics (stderr) so they surface in logs.
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "meet_bot", helper = "meet-audio", "{line}");
                }
            });
        }

        let mut buf = vec![0u8; READ_BUF_BYTES];
        let mut seq: u64 = 0;
        loop {
            match stdout.read(&mut buf).await {
                Ok(0) => break, // helper exited / closed stdout
                Ok(n) => {
                    let chunk = AudioChunk {
                        seq,
                        pcm: Bytes::copy_from_slice(&buf[..n]),
                    };
                    seq = seq.wrapping_add(1);
                    if out.send(chunk).await.is_err() {
                        break; // consumer (STT pump) gone
                    }
                },
                Err(e) => {
                    tracing::warn!(target: "meet_bot", error = %e, "meet-audio helper read error");
                    break;
                },
            }
        }

        let _ = child.start_kill();
        Ok(())
    }
}

/// Captures the user's MICROPHONE (default input device) via the helper's
/// `--mode capture-mic` (AVAudioEngine input → PCM16 stdout) — the passive
/// listener's "You" track. Microphone TCC only; no Screen Recording, no
/// BlackHole. Same chunk contract as [`ScreenCaptureAudioSource`].
pub struct MicrophoneAudioSource {
    helper_path: String,
    format: StreamAudioFormat,
}

impl Default for MicrophoneAudioSource {
    fn default() -> Self {
        Self {
            helper_path: helper_path(),
            format: StreamAudioFormat::default(),
        }
    }
}

impl MicrophoneAudioSource {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl AudioSource for MicrophoneAudioSource {
    async fn run(&self, out: mpsc::Sender<AudioChunk>) -> Result<(), AudioError> {
        let mut child = Command::new(&self.helper_path)
            .arg("--mode")
            .arg("capture-mic")
            .arg("--sample-rate")
            .arg(self.format.sample_rate_hz.to_string())
            .arg("--channels")
            .arg(self.format.channels.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Cancellation drops this run() future (start_kill never runs on
            // that path); without kill_on_drop a stalled-tap helper would keep
            // the microphone open (orange indicator) after the listener stops.
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| AudioError::Device(format!("spawn {}: {e}", self.helper_path)))?;

        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| AudioError::Device("mic-capture helper produced no stdout".into()))?;

        // Drain the helper's JSON diagnostics (stderr) so they surface in logs.
        // Tripwire: the helper taps the system DEFAULT input — if that was
        // left on a loopback device (BlackHole, from the inject side), the
        // "You" track is actually MEETING audio mislabeled as the user.
        // The helper's ready event names the device; make that case loud.
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if line.contains("input_device") && line.to_lowercase().contains("blackhole") {
                        tracing::warn!(
                            target: "meet_bot",
                            helper = "mic-capture",
                            "default input is a LOOPBACK device — the \"You\" track will mislabel meeting audio as the user: {line}"
                        );
                    } else {
                        tracing::debug!(target: "meet_bot", helper = "mic-capture", "{line}");
                    }
                }
            });
        }

        let mut buf = vec![0u8; READ_BUF_BYTES];
        let mut seq: u64 = 0;
        loop {
            match stdout.read(&mut buf).await {
                Ok(0) => break, // helper exited / closed stdout
                Ok(n) => {
                    let chunk = AudioChunk {
                        seq,
                        pcm: Bytes::copy_from_slice(&buf[..n]),
                    };
                    seq = seq.wrapping_add(1);
                    if out.send(chunk).await.is_err() {
                        break; // consumer (STT pump) gone
                    }
                },
                Err(e) => {
                    tracing::warn!(target: "meet_bot", error = %e, "mic-capture helper read error");
                    break;
                },
            }
        }

        let _ = child.start_kill();
        Ok(())
    }
}

/// Injects PCM16 into the meeting mic via the native CoreAudio helper
/// (`magician-macos-meet-audio --mode inject`) — no external dependency. Spawns
/// the helper per reply, writing the PCM to its stdin; the helper plays it to the
/// named device (BlackHole 16ch) then exits.
pub struct CoreAudioSink {
    helper_path: String,
    device: String,
}

impl Default for CoreAudioSink {
    fn default() -> Self {
        Self {
            helper_path: helper_path(),
            device: DEFAULT_INJECT_DEVICE.to_string(),
        }
    }
}

impl CoreAudioSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_device(mut self, device: impl Into<String>) -> Self {
        self.device = device.into();
        self
    }

    pub fn with_helper_path(mut self, path: impl Into<String>) -> Self {
        self.helper_path = path.into();
        self
    }
}

#[async_trait]
impl AudioSink for CoreAudioSink {
    async fn play_pcm(&self, pcm: Bytes, format: StreamAudioFormat) -> Result<(), AudioError> {
        if pcm.is_empty() {
            return Ok(());
        }
        let rate = format.sample_rate_hz.to_string();
        let channels = format.channels.max(1).to_string();
        let mut child = Command::new(&self.helper_path)
            .args([
                "--mode",
                "inject",
                "--device",
                &self.device,
                "--sample-rate",
                &rate,
                "--channels",
                &channels,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Barge-in: when the reply `play` future is dropped (the session's
            // interrupt branch wins), drop this child too and kill the helper so
            // injected audio stops immediately instead of the child lingering and
            // playing out the rest of the buffer.
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| AudioError::Device(format!("spawn inject helper: {e}")))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(&pcm)
                .await
                .map_err(|e| AudioError::Device(format!("inject helper stdin: {e}")))?;
            // Drop stdin → EOF → the helper plays the buffer and exits.
        }

        let status = child
            .wait()
            .await
            .map_err(|e| AudioError::Device(format!("inject helper wait: {e}")))?;
        if !status.success() {
            return Err(AudioError::Device(format!(
                "inject helper exited with {status}"
            )));
        }
        Ok(())
    }
}
