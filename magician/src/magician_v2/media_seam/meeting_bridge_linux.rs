//! Linux audio bridge for the meeting bot.
//!
//! There is no kernel driver. PulseAudio (or PipeWire's Pulse layer) supplies
//! both directions:
//!
//! * **Speak** — [`PulseAudioSink`] writes PCM into the `magician_meet_mic`
//!   null-sink. Its `.monitor` source is the browser's microphone.
//! * **Listen** — [`PulseAudioSource`] records either that meeting's capture
//!   null-sink monitor (browser output moved onto `magician_meet_capture`) or,
//!   when the browser's stream cannot be found, the default sink's monitor
//!   (the desktop mix). Display-monitor capture runs half-duplex, same as
//!   macOS whole-display capture, so the bot does not transcribe itself.
//!
//! Loading a null-sink can become the default output under Pulse's
//! switch-on-connect module. The previous default sink is saved and put back.
//! Leave also returns any browser streams that were moved onto the capture
//! sink. The passive microphone skips `magician_meet_mic.monitor`.
//!
//! The CLIs are `pactl`, `parec`, and `pacat` (`MEET_BOT_PACTL_BIN`,
//! `MEET_BOT_PAREC_BIN`, `MEET_BOT_PACAT_BIN`). `make setup-meet-bot` installs
//! them.

use std::collections::HashSet;
use std::process::Stdio;
use std::sync::Mutex as StdMutex;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex as AsyncMutex};
use tokio::task::JoinHandle;

use crate::magician_v2::media_seam::audio::{AudioError, AudioSink, AudioSource, CaptureTarget};
use crate::magician_v2::media_seam::{AudioChunk, StreamAudioFormat};

pub const CAPTURE_SINK: &str = "magician_meet_capture";
pub const MIC_SINK: &str = "magician_meet_mic";

pub fn capture_monitor() -> String {
    format!("{CAPTURE_SINK}.monitor")
}

pub fn mic_monitor() -> String {
    format!("{MIC_SINK}.monitor")
}

fn pactl_bin() -> String {
    std::env::var("MEET_BOT_PACTL_BIN").unwrap_or_else(|_| "pactl".to_string())
}

fn parec_bin() -> String {
    std::env::var("MEET_BOT_PAREC_BIN").unwrap_or_else(|_| "parec".to_string())
}

fn pacat_bin() -> String {
    std::env::var("MEET_BOT_PACAT_BIN").unwrap_or_else(|_| "pacat".to_string())
}

const READ_BUF_BYTES: usize = 4096;

/// Previous desktop sink, captured before the first null-sink load.
static HOST_DEFAULT_SINK: StdMutex<Option<String>> = StdMutex::new(None);
/// Previous hardware microphone, captured before the virtual mic becomes default.
static HOST_DEFAULT_SOURCE: StdMutex<Option<String>> = StdMutex::new(None);
/// Live attendee holds and the browser pids whose output is on the capture sink.
static MEETING_AUDIO: std::sync::LazyLock<StdMutex<MeetingAudioState>> =
    std::sync::LazyLock::new(|| {
        StdMutex::new(MeetingAudioState {
            holds: 0,
            route_pids: HashSet::new(),
        })
    });
/// Serializes the mover against leave, so a tick cannot re-steal streams after restore.
static CAPTURE_ROUTING_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

/// Process-wide because the null-sinks are process-wide. Each attendee holds
/// one count; only the last leave restores the desktop default devices.
#[derive(Debug)]
struct MeetingAudioState {
    holds: usize,
    route_pids: HashSet<i32>,
}

impl MeetingAudioState {
    fn acquire(&mut self) {
        self.holds = self.holds.saturating_add(1);
    }

    fn begin_route(&mut self, pid: i32) {
        self.route_pids.insert(pid);
    }

    fn end_route(&mut self, pid: i32) {
        self.route_pids.remove(&pid);
    }

    fn release_hold(&mut self) -> bool {
        self.holds = self.holds.saturating_sub(1);
        self.holds == 0 && self.route_pids.is_empty()
    }

    fn idle(&self) -> bool {
        self.holds == 0 && self.route_pids.is_empty()
    }

    fn routing(&self, pid: i32) -> bool {
        self.route_pids.contains(&pid)
    }
}

fn with_meeting_audio<T>(f: impl FnOnce(&mut MeetingAudioState) -> T) -> T {
    let mut guard = MEETING_AUDIO
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut guard)
}

/// One attendee has flipped the default source. Pair with [`release_meeting_routing`].
pub fn acquire_meeting_audio_hold() {
    with_meeting_audio(MeetingAudioState::acquire);
}

/// Held across a default-source flip and across leave's restore so the two
/// cannot overwrite each other.
pub async fn capture_routing_lock() -> tokio::sync::MutexGuard<'static, ()> {
    CAPTURE_ROUTING_LOCK.lock().await
}

pub fn meeting_audio_idle() -> bool {
    with_meeting_audio(|state| state.idle())
}

struct AbortOnDrop {
    handle: JoinHandle<()>,
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn is_meeting_sink(name: &str) -> bool {
    name == CAPTURE_SINK || name == MIC_SINK
}

pub fn is_virtual_meeting_source(name: &str) -> bool {
    let name = name.trim();
    name == mic_monitor() || name == capture_monitor()
}

fn is_meeting_source(name: &str) -> bool {
    is_virtual_meeting_source(name)
}

/// Saved microphone to restore. A meeting monitor is never written back;
/// an empty memory falls through to a real source from `sources`.
pub fn sanitize_saved_source(
    prior: Option<&str>,
    remembered: Option<&str>,
    sources: &[String],
) -> Option<String> {
    let prior = prior.map(str::trim).filter(|name| !name.is_empty());
    match prior {
        Some(name) if usable_microphone(name) => Some(name.to_string()),
        _ => select_user_microphone(prior.unwrap_or(""), remembered, sources),
    }
}

fn remember_host_sink(name: &str) {
    let name = name.trim();
    if name.is_empty() || is_meeting_sink(name) {
        return;
    }
    if let Ok(mut slot) = HOST_DEFAULT_SINK.lock() {
        if slot.is_none() {
            *slot = Some(name.to_string());
        }
    }
}

fn remember_host_source(name: &str) {
    let name = name.trim();
    if name.is_empty() || is_meeting_source(name) {
        return;
    }
    if let Ok(mut slot) = HOST_DEFAULT_SOURCE.lock() {
        if slot.is_none() {
            *slot = Some(name.to_string());
        }
    }
}

fn host_default_sink() -> Option<String> {
    HOST_DEFAULT_SINK.lock().ok().and_then(|slot| slot.clone())
}

fn host_default_source() -> Option<String> {
    HOST_DEFAULT_SOURCE
        .lock()
        .ok()
        .and_then(|slot| slot.clone())
}

fn clear_host_audio_snapshot() {
    if let Ok(mut slot) = HOST_DEFAULT_SINK.lock() {
        *slot = None;
    }
    if let Ok(mut slot) = HOST_DEFAULT_SOURCE.lock() {
        *slot = None;
    }
}

/// Remember a non-virtual default source so a later passive mic can avoid the bot.
pub fn note_host_source(name: &str) {
    remember_host_source(name);
}

/// Sink names from `pactl list short sinks` (second tab-separated field).
pub fn parse_short_sink_names(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let _index = fields.next()?;
            let name = fields.next()?.trim();
            if name.is_empty() {
                None
            } else {
                Some(name.to_string())
            }
        })
        .collect()
}

/// Indices of sink-inputs whose `application.process.id` is one of `pids`.
///
/// `pactl list sink-inputs` blocks look like:
///
/// ```text
/// Sink Input #67
///     Properties:
///         application.process.id = "4242"
/// ```
pub fn parse_sink_input_indices(stdout: &str, pids: &[i32]) -> Vec<u32> {
    let mut matched = Vec::new();
    let mut current: Option<u32> = None;
    for line in stdout.lines() {
        if let Some(index) = line.trim().strip_prefix("Sink Input #") {
            if let Ok(index) = index.trim().parse::<u32>() {
                current = Some(index);
            }
            continue;
        }
        let Some(index) = current else {
            continue;
        };
        let Some(pid) = process_id_on_line(line) else {
            continue;
        };
        if pids.contains(&pid) {
            matched.push(index);
            current = None;
        }
    }
    matched
}

fn process_id_on_line(line: &str) -> Option<i32> {
    let (_, rest) = line.split_once("application.process.id")?;
    let (_, value) = rest.split_once('=')?;
    let value = value.trim().trim_matches('"').trim();
    value.parse().ok()
}

/// Indices whose `Sink:` line names `sink_name` (`Sink: 52 <magician_meet_capture>`).
pub fn parse_sink_inputs_on_sink(stdout: &str, sink_name: &str) -> Vec<u32> {
    let mut matched = Vec::new();
    let mut current: Option<u32> = None;
    let mut on_sink = false;
    for line in stdout.lines() {
        if let Some(rest) = line.trim().strip_prefix("Sink Input #") {
            if on_sink {
                if let Some(index) = current {
                    matched.push(index);
                }
            }
            current = rest.trim().parse().ok();
            on_sink = false;
            continue;
        }
        if current.is_none() {
            continue;
        }
        let Some(rest) = line.trim().strip_prefix("Sink:") else {
            continue;
        };
        on_sink = sink_line_names(rest.trim(), sink_name);
    }
    if on_sink {
        if let Some(index) = current {
            matched.push(index);
        }
    }
    matched
}

fn sink_line_names(rest: &str, sink_name: &str) -> bool {
    if rest == sink_name {
        return true;
    }
    if let Some(start) = rest.find('<') {
        if let Some(end) = rest[start + 1..].find('>') {
            return &rest[start + 1..start + 1 + end] == sink_name;
        }
    }
    false
}

/// Pick the human microphone. The meeting's virtual mic is a `.monitor` source
/// and must not become the passive "You" track.
pub fn select_user_microphone(
    current_default: &str,
    remembered: Option<&str>,
    sources: &[String],
) -> Option<String> {
    let current = current_default.trim();
    if usable_microphone(current) {
        return Some(current.to_string());
    }
    if let Some(saved) = remembered
        .map(str::trim)
        .filter(|name| usable_microphone(name))
    {
        return Some(saved.to_string());
    }
    sources.iter().find(|name| usable_microphone(name)).cloned()
}

fn usable_microphone(name: &str) -> bool {
    !name.is_empty() && !name.ends_with(".monitor") && !is_meeting_source(name)
}

/// Hardware sink to record when capture is not isolated. Never our null-sinks.
pub fn select_playback_sink(
    current_default: &str,
    remembered: Option<&str>,
    sinks: &[String],
) -> Option<String> {
    let current = current_default.trim();
    if !current.is_empty() && !is_meeting_sink(current) {
        return Some(current.to_string());
    }
    if let Some(saved) = remembered
        .map(str::trim)
        .filter(|name| !name.is_empty() && !is_meeting_sink(name))
    {
        return Some(saved.to_string());
    }
    sinks.iter().find(|name| !is_meeting_sink(name)).cloned()
}

/// Records meeting audio with `parec`.
pub struct PulseAudioSource {
    target: std::sync::Mutex<CaptureTarget>,
    format: StreamAudioFormat,
    microphone: bool,
}

impl Default for PulseAudioSource {
    fn default() -> Self {
        Self {
            target: std::sync::Mutex::new(CaptureTarget::DisplayAudio),
            format: StreamAudioFormat::default(),
            microphone: false,
        }
    }
}

impl PulseAudioSource {
    pub fn new() -> Self {
        Self::default()
    }

    /// The machine's real microphone. Skips the meeting virtual mic.
    pub fn microphone() -> Self {
        Self {
            microphone: true,
            ..Self::default()
        }
    }

    pub fn with_format(mut self, format: StreamAudioFormat) -> Self {
        self.format = format;
        self
    }

    async fn capture_device(
        &self,
        target: &CaptureTarget,
    ) -> Result<(String, Option<AbortOnDrop>), AudioError> {
        if self.microphone {
            return Ok((user_microphone_device().await?, None));
        }
        match target {
            CaptureTarget::Pid(pid) => {
                ensure_meeting_devices().await.map_err(AudioError::Device)?;
                match route_sink_inputs_for_pid(*pid).await {
                    Ok(0) => {
                        with_meeting_audio(|state| state.end_route(*pid));
                        tracing::info!(
                            target: "meet_bot",
                            pid,
                            "no browser sink-input; recording the default sink monitor"
                        );
                        Ok((default_sink_monitor().await?, None))
                    },
                    Ok(moved) => {
                        tracing::info!(
                            target: "meet_bot",
                            pid,
                            moved,
                            "recording the capture monitor"
                        );
                        with_meeting_audio(|state| state.begin_route(*pid));
                        Ok((capture_monitor(), Some(spawn_route_mover(*pid))))
                    },
                    Err(error) => {
                        // The joiner may already have moved streams. Stay on the
                        // capture monitor instead of recording a sink they left.
                        tracing::warn!(
                            target: "meet_bot",
                            pid,
                            %error,
                            "Pulse routing failed; recording the capture monitor"
                        );
                        with_meeting_audio(|state| state.begin_route(*pid));
                        Ok((capture_monitor(), Some(spawn_route_mover(*pid))))
                    },
                }
            },
            CaptureTarget::DisplayAudio | CaptureTarget::BundleId(_) => {
                Ok((default_sink_monitor().await?, None))
            },
        }
    }
}

#[async_trait]
impl AudioSource for PulseAudioSource {
    async fn set_capture_target(&self, target: CaptureTarget) {
        *self.target.lock().unwrap() = target;
    }

    async fn run(&self, out: mpsc::Sender<AudioChunk>) -> Result<(), AudioError> {
        let target = self.target.lock().unwrap().clone();
        // Held until this future ends, including cancel, so the mover cannot
        // keep moving browser audio onto the capture sink after leave.
        let (device, _routing) = self.capture_device(&target).await?;
        if device.is_empty() {
            return Err(AudioError::Device(
                "Pulse capture device name was empty".into(),
            ));
        }

        let rate = self.format.sample_rate_hz.to_string();
        let channels = self.format.channels.max(1).to_string();
        let format_arg = format!("--format=s16le");
        let rate_arg = format!("--rate={rate}");
        let channels_arg = format!("--channels={channels}");
        let bin = parec_bin();
        let mut child = Command::new(&bin)
            .args([
                "--raw",
                &format_arg,
                &rate_arg,
                &channels_arg,
                "-d",
                &device,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                AudioError::Device(format!(
                    "spawn {bin} ({e}). Install Pulse tools with `make setup-meet-bot`."
                ))
            })?;

        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "meet_bot", helper = "parec", "{line}");
                }
            });
        }

        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| AudioError::Device("parec produced no stdout".into()))?;
        let mut buf = vec![0u8; READ_BUF_BYTES];
        let mut seq: u64 = 0;
        loop {
            match stdout.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = AudioChunk {
                        seq,
                        pcm: Bytes::copy_from_slice(&buf[..n]),
                    };
                    seq = seq.wrapping_add(1);
                    if out.send(chunk).await.is_err() {
                        break;
                    }
                },
                Err(e) => {
                    tracing::warn!(target: "meet_bot", error = %e, "parec read error");
                    break;
                },
            }
        }
        let _ = child.start_kill();
        Ok(())
    }
}

/// Plays PCM into the virtual meeting microphone.
pub struct PulseAudioSink;

impl PulseAudioSink {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PulseAudioSink {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AudioSink for PulseAudioSink {
    async fn play_pcm(&self, pcm: Bytes, format: StreamAudioFormat) -> Result<(), AudioError> {
        if pcm.is_empty() {
            return Ok(());
        }
        ensure_meeting_devices().await.map_err(AudioError::Device)?;
        let rate = format.sample_rate_hz.to_string();
        let channels = format.channels.max(1).to_string();
        let format_arg = format!("--format=s16le");
        let rate_arg = format!("--rate={rate}");
        let channels_arg = format!("--channels={channels}");
        let bin = pacat_bin();
        let mut child = Command::new(&bin)
            .args([
                "--raw",
                "--playback",
                &format_arg,
                &rate_arg,
                &channels_arg,
                "-d",
                MIC_SINK,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                AudioError::Device(format!(
                    "spawn {bin} ({e}). Install Pulse tools with `make setup-meet-bot`."
                ))
            })?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(&pcm)
                .await
                .map_err(|e| AudioError::Device(format!("pacat stdin: {e}")))?;
        }
        let status = child
            .wait()
            .await
            .map_err(|e| AudioError::Device(format!("pacat wait: {e}")))?;
        if !status.success() {
            return Err(AudioError::Device(format!("pacat exited with {status}")));
        }
        Ok(())
    }
}

/// Create the capture sink and the virtual microphone if they are absent.
///
/// Pulse `module-switch-on-connect` makes a newly loaded sink the default
/// output. The previous sink is put back before this returns.
pub async fn ensure_meeting_devices() -> Result<(), String> {
    let listed = pactl(&["list", "short", "sinks"]).await?;
    let names = parse_short_sink_names(&listed);
    if let Ok(current) = pactl(&["get-default-sink"]).await {
        remember_host_sink(current.trim());
    }
    let mut load_error = None;
    if !names.iter().any(|name| name == CAPTURE_SINK) {
        if let Err(error) = load_null_sink(CAPTURE_SINK, "MagicianMeetCapture").await {
            load_error = Some(error);
        }
    }
    if load_error.is_none() && !names.iter().any(|name| name == MIC_SINK) {
        if let Err(error) = load_null_sink(MIC_SINK, "MagicianMeetMic").await {
            load_error = Some(error);
        }
    }
    // A partial load can already have stolen the default sink.
    let restored = restore_host_sink_now(&names).await;
    if let Some(error) = load_error {
        return Err(error);
    }
    restored
}

async fn load_null_sink(sink_name: &str, description: &str) -> Result<(), String> {
    let name_arg = format!("sink_name={sink_name}");
    // One argv element. Spaces inside would become extra module arguments and
    // `module-null-sink` would refuse to load.
    let props = format!("sink_properties=device.description={description}");
    pactl(&["load-module", "module-null-sink", &name_arg, &props]).await?;
    Ok(())
}

/// Drop one attendee's hold. Move only that browser's streams off the capture
/// sink. Restore the desktop default devices only when no attendee remains.
pub async fn release_meeting_routing(browser_pid: Option<i32>) -> Result<(), String> {
    if let Some(pid) = browser_pid {
        with_meeting_audio(|state| state.end_route(pid));
    }
    with_meeting_audio(MeetingAudioState::release_hold);
    let _guard = CAPTURE_ROUTING_LOCK.lock().await;
    let inputs = match browser_pid {
        Some(pid) => restore_routed_inputs_for_pid(pid).await,
        None => Ok(()),
    };
    // Re-check after the lock so a join that arrived mid-restore keeps the devices.
    let last = with_meeting_audio(|state| state.idle());
    if !last {
        return inputs;
    }
    let sink = restore_host_sink_now(&[]).await;
    let source = restore_host_source_now().await;
    clear_host_audio_snapshot();
    inputs.and(sink).and(source)
}

fn spawn_route_mover(pid: i32) -> AbortOnDrop {
    let handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            if !with_meeting_audio(|state| state.routing(pid)) {
                break;
            }
            let _guard = CAPTURE_ROUTING_LOCK.lock().await;
            if !with_meeting_audio(|state| state.routing(pid)) {
                break;
            }
            let _ = route_sink_inputs_for_pid(pid).await;
        }
    });
    AbortOnDrop { handle }
}

async fn default_sink_monitor() -> Result<String, AudioError> {
    let current = pactl(&["get-default-sink"])
        .await
        .map_err(AudioError::Device)?;
    let listed = pactl(&["list", "short", "sinks"])
        .await
        .map_err(AudioError::Device)?;
    let sinks = parse_short_sink_names(&listed);
    let remembered = host_default_sink();
    let sink = select_playback_sink(current.trim(), remembered.as_deref(), &sinks).ok_or_else(|| {
        AudioError::Device(
            "Pulse has no default sink. Start PipeWire or PulseAudio, then re-run make setup-meet-bot.".into(),
        )
    })?;
    Ok(format!("{sink}.monitor"))
}

async fn user_microphone_device() -> Result<String, AudioError> {
    let current = pactl(&["get-default-source"])
        .await
        .map_err(AudioError::Device)?;
    let listed = pactl(&["list", "short", "sources"])
        .await
        .unwrap_or_default();
    let sources = parse_short_sink_names(&listed);
    let remembered = host_default_source();
    select_user_microphone(current.trim(), remembered.as_deref(), &sources).ok_or_else(|| {
        AudioError::Device(
            "Pulse has no hardware microphone. The meeting virtual mic is not used for the passive mic.".into(),
        )
    })
}

async fn restore_host_sink_now(known_sinks: &[String]) -> Result<(), String> {
    let current = pactl(&["get-default-sink"]).await?;
    let current = current.trim();
    if !current.is_empty() && !is_meeting_sink(current) {
        return Ok(());
    }
    let sink_list = if known_sinks.is_empty() {
        let listed = pactl(&["list", "short", "sinks"]).await?;
        parse_short_sink_names(&listed)
    } else {
        known_sinks.to_vec()
    };
    let remembered = host_default_sink();
    let Some(dest) = select_playback_sink(current, remembered.as_deref(), &sink_list) else {
        return Ok(());
    };
    if dest == current {
        return Ok(());
    }
    pactl(&["set-default-sink", &dest]).await?;
    Ok(())
}

async fn restore_host_source_now() -> Result<(), String> {
    let current = pactl(&["get-default-source"]).await?;
    let listed = pactl(&["list", "short", "sources"]).await?;
    let sources = parse_short_sink_names(&listed);
    let remembered = host_default_source();
    let Some(dest) = sanitize_saved_source(Some(current.trim()), remembered.as_deref(), &sources)
    else {
        return Ok(());
    };
    if dest == current.trim() || is_meeting_source(&dest) {
        return Ok(());
    }
    pactl(&["set-default-source", &dest]).await?;
    Ok(())
}

/// Microphone to put back on leave. Never `magician_meet_mic.monitor`.
pub async fn host_source_to_restore(prior: Option<&str>) -> Result<Option<String>, String> {
    let listed = pactl(&["list", "short", "sources"]).await?;
    let sources = parse_short_sink_names(&listed);
    let remembered = host_default_source();
    Ok(sanitize_saved_source(
        prior,
        remembered.as_deref(),
        &sources,
    ))
}

async fn restore_routed_inputs_for_pid(pid: i32) -> Result<(), String> {
    let pids = descendant_pids(pid).await;
    let listed_sinks = pactl(&["list", "short", "sinks"]).await?;
    let sinks = parse_short_sink_names(&listed_sinks);
    let current = pactl(&["get-default-sink"]).await.unwrap_or_default();
    let remembered = host_default_sink();
    let Some(dest) = select_playback_sink(current.trim(), remembered.as_deref(), &sinks) else {
        return Ok(());
    };
    let listed = pactl(&["list", "sink-inputs"]).await?;
    let on_capture: HashSet<u32> = parse_sink_inputs_on_sink(&listed, CAPTURE_SINK)
        .into_iter()
        .collect();
    for index in parse_sink_input_indices(&listed, &pids) {
        if !on_capture.contains(&index) {
            continue;
        }
        let index_arg = index.to_string();
        pactl(&["move-sink-input", &index_arg, &dest]).await?;
    }
    Ok(())
}

/// Move every sink-input owned by `pid` or a descendant onto the capture sink.
/// Returns how many inputs moved.
pub async fn route_sink_inputs_for_pid(pid: i32) -> Result<usize, String> {
    ensure_meeting_devices().await?;
    let pids = descendant_pids(pid).await;
    let listed = pactl(&["list", "sink-inputs"]).await?;
    let indices = parse_sink_input_indices(&listed, &pids);
    let count = indices.len();
    for index in indices {
        pactl(&["move-sink-input", &index.to_string(), CAPTURE_SINK]).await?;
    }
    Ok(count)
}

async fn descendant_pids(root: i32) -> Vec<i32> {
    let mut all = vec![root];
    let mut queue = vec![root];
    while let Some(parent) = queue.pop() {
        let Ok(out) = Command::new("ps")
            .args(["-o", "pid=", "--ppid", &parent.to_string()])
            .output()
            .await
        else {
            continue;
        };
        if !out.status.success() {
            continue;
        }
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let Ok(pid) = line.trim().parse::<i32>() else {
                continue;
            };
            if !all.contains(&pid) {
                all.push(pid);
                queue.push(pid);
            }
        }
    }
    all
}

async fn pactl(args: &[&str]) -> Result<String, String> {
    let bin = pactl_bin();
    let out = Command::new(&bin).args(args).output().await.map_err(|e| {
        format!("{bin} is not runnable ({e}). Install Pulse tools with `make setup-meet-bot`.")
    })?;
    if !out.status.success() {
        return Err(format!(
            "{bin} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_sink_names_read_the_second_field() {
        let stdout = "43\tmagician_meet_capture\tmodule-null-sink.c\tSUSPENDED\n\
                      44\talsa_output.pci\tmodule-alsa-card.c\tRUNNING\n";
        assert_eq!(
            parse_short_sink_names(stdout),
            vec![
                "magician_meet_capture".to_string(),
                "alsa_output.pci".to_string()
            ]
        );
    }

    #[test]
    fn sink_inputs_match_only_listed_pids() {
        let stdout = "\
Sink Input #12
    Sink: 1
    Properties:
        application.process.id = \"999\"
        application.name = \"Other\"
Sink Input #67
    Sink: 2
    Properties:
        application.name = \"Chromium\"
        application.process.id = \"4242\"
Sink Input #68
    Properties:
        application.process.id = 4243
";
        assert_eq!(
            parse_sink_input_indices(stdout, &[4242, 4243]),
            vec![67, 68]
        );
        assert!(parse_sink_input_indices(stdout, &[1]).is_empty());
    }

    #[test]
    fn sink_inputs_on_the_capture_sink_use_the_angle_name() {
        let stdout = "\
Sink Input #12
    Sink: 1 <alsa_output.pci>
Sink Input #67
    Sink: 52 <magician_meet_capture>
Sink Input #68
    Sink: magician_meet_mic
";
        assert_eq!(parse_sink_inputs_on_sink(stdout, CAPTURE_SINK), vec![67]);
        assert!(parse_sink_inputs_on_sink(stdout, "alsa_output.pci.monitor").is_empty());
    }

    #[test]
    fn user_microphone_skips_the_virtual_mic_and_monitors() {
        let sources = vec![
            "magician_meet_mic.monitor".to_string(),
            "magician_meet_capture.monitor".to_string(),
            "alsa_output.pci.monitor".to_string(),
            "alsa_input.pci.analog-stereo".to_string(),
        ];
        assert_eq!(
            select_user_microphone(
                "magician_meet_mic.monitor",
                Some("alsa_input.pci.analog-stereo"),
                &sources,
            )
            .as_deref(),
            Some("alsa_input.pci.analog-stereo")
        );
        assert_eq!(
            select_user_microphone("alsa_input.usb", None, &sources).as_deref(),
            Some("alsa_input.usb")
        );
        assert_eq!(
            select_user_microphone("alsa_output.pci.monitor", None, &sources).as_deref(),
            Some("alsa_input.pci.analog-stereo")
        );
    }

    #[test]
    fn playback_sink_skips_meeting_null_sinks() {
        let sinks = vec![
            CAPTURE_SINK.to_string(),
            MIC_SINK.to_string(),
            "alsa_output.pci".to_string(),
        ];
        assert_eq!(
            select_playback_sink(MIC_SINK, Some("alsa_output.pci"), &sinks).as_deref(),
            Some("alsa_output.pci")
        );
        assert_eq!(
            select_playback_sink("alsa_output.usb", Some("alsa_output.pci"), &sinks).as_deref(),
            Some("alsa_output.usb")
        );
        assert_eq!(
            select_playback_sink(CAPTURE_SINK, None, &sinks).as_deref(),
            Some("alsa_output.pci")
        );
    }

    #[test]
    fn saved_source_never_restores_the_virtual_mic() {
        let sources = vec![
            "magician_meet_mic.monitor".to_string(),
            "alsa_output.pci.monitor".to_string(),
            "alsa_input.pci.analog-stereo".to_string(),
        ];
        assert_eq!(
            sanitize_saved_source(Some("magician_meet_mic.monitor"), None, &sources).as_deref(),
            Some("alsa_input.pci.analog-stereo")
        );
        assert_eq!(
            sanitize_saved_source(
                Some("alsa_input.usb"),
                Some("alsa_input.pci.analog-stereo"),
                &sources,
            )
            .as_deref(),
            Some("alsa_input.usb")
        );
        assert_eq!(
            sanitize_saved_source(None, Some("alsa_input.pci.analog-stereo"), &sources).as_deref(),
            Some("alsa_input.pci.analog-stereo")
        );
    }

    #[test]
    fn leaving_one_meeting_keeps_the_other_routed() {
        let mut state = MeetingAudioState {
            holds: 0,
            route_pids: std::collections::HashSet::new(),
        };
        state.acquire();
        state.acquire();
        state.begin_route(10);
        state.begin_route(20);
        state.end_route(10);
        assert!(!state.release_hold());
        assert!(state.routing(20));
        assert!(!state.idle());
        state.end_route(20);
        assert!(state.release_hold());
        assert!(state.idle());
    }
}
