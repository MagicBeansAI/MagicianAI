//! `BrowserJoin` — the seam that joins/leaves a Google Meet in a browser.
//!
//! Owned by `MeetingSession` (Option X) so all browser/host coupling lives in
//! one swappable place. Two impls:
//!
//! * [`NoopBrowserJoin`] (default, cross-platform) — does nothing. The bot rides
//!   an already-open browser (the Phase 0/1 manual-join path); capture keeps its
//!   bundle-id default and there is no auto-teardown.
//! * [`AgentBrowserMeetJoiner`] (macOS and Linux) — launches the bot's *own* headed
//!   browser via [`AgentBrowserSession`], runs the Meet pre-join flow, and points
//!   the meeting microphone at the inject device (BlackHole 16ch on macOS, the
//!   Pulse `magician_meet_mic.monitor` source on Linux). Restored on leave.
//!   macOS capture stays ScreenCaptureKit. Linux moves the browser's Pulse
//!   sink-input onto `magician_meet_capture` when it can, and otherwise records
//!   the default sink monitor.
//!
//! **Live-tuning warning.** The exact Meet pre-join accessibility labels/roles
//! are not known until this is run against a real Meet. The join flow is written
//! to be *generic* (search the snapshot for role/visible-text rather than brittle
//! CSS) and *heavily logged* so the live gate can correct the selector guesses.
//! Every place that guesses a Meet-specific string is isolated in a clearly
//! commented `const` near the top of the macOS block.
//!
//! See `docs/plans/2026-06-09-meet-bot-phase2-autojoin.md` (Task 2).

use async_trait::async_trait;

use crate::magician_v2::media_seam::audio::CaptureTarget;

/// What a successful join hands back to the session.
#[derive(Debug, Clone, Default)]
pub struct JoinedMeeting {
    /// PID of the launched browser, so ScreenCaptureKit captures THIS instance.
    /// `None` → no-op join (the bot rides an already-open browser; capture keeps
    /// its bundle-id default).
    pub capture_target: Option<CaptureTarget>,
}

/// Joins/leaves the meeting in a browser. Owned by `MeetingSession` (Option X)
/// so all browser coupling is one swappable seam; the no-op default keeps the
/// session testable and preserves the manual-join (ride-an-open-Chrome) path.
#[async_trait]
pub trait BrowserJoin: Send + Sync {
    /// Launch + join the meeting. Returns the launched browser's PID (when a real
    /// browser was launched) so the session can retarget capture to it.
    async fn join(&self, meet_url: &str, display_name: &str) -> Result<JoinedMeeting, String>;

    /// Tear down: leave the call, close the browser, restore host audio.
    async fn leave(&self);

    /// Poll the meeting for "removed / meeting ended". Default `true` = never
    /// auto-teardown (correct for the no-op / manual-join path; the real joiner
    /// overrides this to drive removed-detection).
    async fn is_in_meeting(&self) -> bool {
        true
    }
}

/// Default seam: do nothing. The bot rides an already-joined browser (Phase 0/1
/// manual-join). No PID → capture keeps its bundle-id default; `is_in_meeting`
/// stays `true` so the session never auto-tears-down on this path.
pub struct NoopBrowserJoin;

#[async_trait]
impl BrowserJoin for NoopBrowserJoin {
    async fn join(&self, _meet_url: &str, _display_name: &str) -> Result<JoinedMeeting, String> {
        Ok(JoinedMeeting::default())
    }

    async fn leave(&self) {}
}

/// Pick the first PID from `pgrep` stdout (one PID per line). Tolerates blank
/// lines and non-numeric junk; returns `None` if no line parses to an `i32`.
pub(crate) fn parse_first_pid(pgrep_stdout: &str) -> Option<i32> {
    pgrep_stdout
        .lines()
        .filter_map(|l| l.trim().parse::<i32>().ok())
        .next()
}

// ---------------------------------------------------------------------------
// macOS real joiner
// ---------------------------------------------------------------------------

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use macos::AgentBrowserMeetJoiner;

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod macos {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use async_trait::async_trait;
    use tokio::sync::Mutex;
    use tracing::{error, info, warn};

    use crate::magician_v2::artifact_v2::io::write_bytes_durably;
    use crate::magician_v2::execution::primitive_dispatch::browser::session::{
        AgentBrowserSession, BrowserEnginePlan, ConnectionMode,
    };
    use crate::magician_v2::media_seam::audio::CaptureTarget;
    use crate::magician_v2::media_seam::meeting_bridge_macos::DEFAULT_INJECT_DEVICE;
    use crate::magician_v2::media_seam::{parse_first_pid, BrowserJoin, JoinedMeeting};

    // --- Meet-specific guesses (LIVE-TUNE these against a real Meet) ----------
    //
    // The Meet pre-join page exposes a "Your name" textbox and an "Ask to join"
    // (or "Join now" when you're the host) button. The exact accessible names
    // vary by Meet build / locale, so we match a *set* of candidate substrings
    // case-insensitively against the snapshot rather than pinning one CSS path.
    // If the live gate shows we picked the wrong control, extend these lists.

    /// Visible-name substrings that identify the "Your name" pre-join field.
    const NAME_FIELD_CANDIDATES: &[&str] = &["your name", "name"];
    /// Visible-name substrings that identify the join button.
    const JOIN_BUTTON_CANDIDATES: &[&str] = &["ask to join", "join now", "join meeting", "join"];
    /// Visible-name substrings that identify the in-call leave/hang-up control.
    /// Presence of one of these is our "we're admitted / in the call" signal.
    const LEAVE_BUTTON_CANDIDATES: &[&str] = &["leave call", "leave the call", "leave meeting"];
    /// Snapshot text shown AFTER our knock registered — we asked to join and are
    /// waiting for the host. Presence means "stop clicking, just keep waiting".
    const WAITING_TEXT_CANDIDATES: &[&str] = &[
        "asking to be let in",
        "you'll join the call when",
        "you will join the call when",
        "someone lets you in",
        "someone will let you in",
        "waiting for the host",
        "wait for someone to let you in",
        "let you in soon",
    ];
    /// Pre-admission TERMINAL text: the knock was denied or the meeting is gone.
    /// Matching any of these aborts the join immediately instead of burning the
    /// whole admission window re-clicking a page that has no join control (and
    /// spamming the host with repeated knocks). Only message texts that never
    /// appear on a joinable page belong here — "Return to home screen" does NOT
    /// (the healthy signed-in pre-join page carries it as a nav link; live gate
    /// 2026-06-10 false-aborted on exactly that) — and the check is additionally
    /// gated on the page having NO join control.
    const PREJOIN_TERMINAL_TEXT_CANDIDATES: &[&str] = &[
        "you can't join this call",
        "can't join this call",
        "denied your request",
        "you weren't let in",
        "no one let you in",
        "you've been removed",
        "removed from the meeting",
        "the meeting has ended",
    ];
    /// Snapshot text that means we were removed / the meeting ended.
    const REMOVED_TEXT_CANDIDATES: &[&str] = &[
        "you've left",
        "you have left",
        "removed from the meeting",
        "you've been removed",
        "the meeting has ended",
        "meeting ended",
        "no one else is here",
    ];

    /// How long to wait for the host to admit the bot before giving up. The
    /// window restarts when the knock registers (waiting page detected), so
    /// slow pre-join driving never eats the host's admission time.
    const ADMIT_TIMEOUT_SECS: u64 = 120;
    /// Poll interval while waiting for admission.
    const ADMIT_POLL_INTERVAL_SECS: u64 = 2;
    /// Minimum spacing between join-click attempts. Re-clicking is the
    /// self-heal for a first click that didn't take, but unthrottled it can
    /// re-knock every poll (spamming the host with admission requests when the
    /// waiting-page text isn't recognized, e.g. a non-English Meet UI).
    const CLICK_RETRY_COOLDOWN: Duration = Duration::from_secs(6);

    /// Launches the bot's own headed browser, joins a Meet, and discovers the
    /// launched browser's PID for PID-targeted capture.
    ///
    /// `user_data_dir` MUST be unique per joiner (constructed by the caller in
    /// Task 3, e.g. a uuid-keyed scope dir). It is the disambiguator that lets
    /// `pgrep` find *this* Chromium among any others.
    pub struct AgentBrowserMeetJoiner {
        cli_path: PathBuf,
        user_data_dir: String,
        /// Primary browser-engine env plus the same bounded capacity fallback
        /// used by ordinary browser calls. This keeps meeting joins aligned
        /// with the regular browser tool.
        engine_plan: BrowserEnginePlan,
        analytics:
            Option<crate::magician_v2::browser_engine_analytics::BrowserEngineAnalyticsContext>,
        session: Mutex<Option<AgentBrowserSession>>,
        /// The host default input device captured before we flipped it, so
        /// `leave()` can restore it.
        prior_input: Mutex<Option<String>>,
        /// Browser pid whose sink-inputs were moved onto the capture sink.
        /// Leave moves only this pid back. Unused on macOS.
        #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
        routed_pid: Mutex<Option<i32>>,
    }

    impl AgentBrowserMeetJoiner {
        /// `cli_path` — the pinned agent-browser binary (from
        /// `AgentBrowserSession::resolve_cli_path*`). `user_data_dir` — a UNIQUE
        /// Chrome `--user-data-dir` for this joiner.
        pub fn new(
            cli_path: PathBuf,
            user_data_dir: String,
            engine_plan: BrowserEnginePlan,
            analytics: Option<
                crate::magician_v2::browser_engine_analytics::BrowserEngineAnalyticsContext,
            >,
        ) -> Self {
            Self {
                cli_path,
                user_data_dir,
                engine_plan,
                analytics,
                session: Mutex::new(None),
                prior_input: Mutex::new(None),
                routed_pid: Mutex::new(None),
            }
        }

        /// Read the snapshot's `[ref=eN]` for the first line whose visible name
        /// contains one of `candidates` (case-insensitive). Snapshot lines look
        /// like `- textbox "Your name" [ref=e3]` (see agent-browser README ::
        /// Selectors). Returns the `@eN` ref ready to pass to `click`/`fill`.
        fn find_ref(snapshot: &str, candidates: &[&str]) -> Option<String> {
            // Candidates are ordered specific → generic; honor that order so a
            // generic substring (e.g. bare "join") can't shadow the real control
            // ("Ask to join") when both appear in the snapshot.
            for c in candidates {
                for line in snapshot.lines() {
                    let lower = line.to_lowercase();
                    if lower.contains(c) {
                        if let Some(r) = extract_ref(line) {
                            return Some(format!("@{r}"));
                        }
                    }
                }
            }
            None
        }

        /// Restore the host's prior default input device (no-op if never flipped
        /// or already restored). EVERY `join()` failure path must run this — the
        /// mic is flipped to BlackHole as step 1, and leaving it flipped silences
        /// every other app's microphone until manual intervention.
        async fn restore_prior_input(&self) {
            #[cfg(target_os = "linux")]
            {
                let pid = self.routed_pid.lock().await.take();
                // Release restores the desktop mic itself, and only when this
                // was the last attendee. Writing `prior_input` here would put
                // the virtual mic back, or steal the mic from a meeting that
                // is still live.
                if let Err(e) =
                    crate::magician_v2::media_seam::meeting_bridge_linux::release_meeting_routing(
                        pid,
                    )
                    .await
                {
                    warn!(error = %e, "meet-bot: failed to restore Pulse routing");
                }
                self.prior_input.lock().await.take();
                return;
            }
            if let Some(p) = self.prior_input.lock().await.take() {
                info!(restored_input = %p, "meet-bot: restoring prior default input device");
                if let Err(e) = set_default_input(&p).await {
                    warn!(error = %e, "meet-bot: failed to restore prior input device");
                }
            }
        }
    }

    /// Resolve the `.app` bundle identifier for a browser executable like
    /// `…/Foo.app/Contents/MacOS/Foo`, by reading `CFBundleIdentifier` from the
    /// app's `Info.plist`. This lets SCK audio capture target the RIGHT application
    /// derived from whatever browser the engine resolver actually launched — so
    /// there's no hardcoded browser→bundle map to drift when the engine changes.
    /// macOS-only (PlistBuddy ships with the OS). Async (tokio process) so the
    /// spawn never blocks a runtime worker thread.
    async fn bundle_id_for_executable(exe: &str) -> Option<String> {
        let info_plist = std::path::Path::new(exe)
            .ancestors()
            .find(|a| a.extension().and_then(|e| e.to_str()) == Some("app"))?
            .join("Contents")
            .join("Info.plist");
        let out = tokio::process::Command::new("/usr/libexec/PlistBuddy")
            .arg("-c")
            .arg("Print :CFBundleIdentifier")
            .arg(&info_plist)
            .output()
            .await
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!id.is_empty()).then_some(id)
    }

    /// Resolve the bundle id of the app that owns `pid` via its executable path
    /// (`ps -o comm=` prints the full binary path on macOS). Covers the
    /// no-engine-override launch, where the executable path isn't in our env but
    /// the launched browser's PID is known.
    async fn bundle_id_for_pid(pid: i32) -> Option<String> {
        let out = tokio::process::Command::new("/bin/ps")
            .args(["-o", "comm=", "-p", &pid.to_string()])
            .output()
            .await
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let exe = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if exe.is_empty() {
            return None;
        }
        bundle_id_for_executable(&exe).await
    }

    /// Click the join control by visible text, trying the candidate labels in
    /// order. Returns `true` only when a click actually landed (command ran AND
    /// reported success) — callers use this to decide whether a knock happened.
    async fn click_join_by_text(session: &AgentBrowserSession, session_id: &str) -> bool {
        for label in ["Ask to join", "Join now"] {
            let landed = session
                .run_command(&["find", "text", label, "click"])
                .await
                .map(|r| r.success)
                .unwrap_or(false);
            if landed {
                info!(session_id = %session_id, label, "meet-join: clicked join control by text");
                return true;
            }
        }
        false
    }

    /// Extract the `eN` token from a snapshot line containing `[ref=eN]`.
    fn extract_ref(line: &str) -> Option<String> {
        let start = line.find("[ref=")? + "[ref=".len();
        let rest = &line[start..];
        let end = rest.find(']')?;
        let token = rest[..end].trim();
        if token.is_empty() {
            None
        } else {
            Some(token.to_string())
        }
    }

    /// Read the current default input device name (to restore on leave).
    async fn current_default_input() -> Option<String> {
        #[cfg(target_os = "macos")]
        {
            let out = tokio::process::Command::new("SwitchAudioSource")
                .args(["-t", "input", "-c"])
                .output()
                .await
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if name.is_empty() {
                None
            } else {
                Some(name)
            }
        }
        #[cfg(target_os = "linux")]
        {
            let out = tokio::process::Command::new(
                std::env::var("MEET_BOT_PACTL_BIN").unwrap_or_else(|_| "pactl".into()),
            )
            .args(["get-default-source"])
            .output()
            .await
            .ok()?;
            if !out.status.success() {
                return None;
            }
            let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if name.is_empty() {
                None
            } else {
                Some(name)
            }
        }
    }

    /// Point the meeting microphone at the inject device.
    /// macOS: `SwitchAudioSource -t input -s <name>` (BlackHole 16ch).
    /// Linux: the Pulse virtual-mic monitor, unless `name` is a saved source
    /// being restored.
    async fn set_default_input(name: &str) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            let out = tokio::process::Command::new("SwitchAudioSource")
                .args(["-t", "input", "-s", name])
                .output()
                .await
                .map_err(|e| {
                    format!(
                        "SwitchAudioSource not runnable ({e}); install it via \
                         `brew install switchaudio-osx` (a setup-meet-bot.sh dep)"
                    )
                })?;
            if out.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "SwitchAudioSource failed to set input `{name}`: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        }
        #[cfg(target_os = "linux")]
        {
            let inject = name == DEFAULT_INJECT_DEVICE;
            if !inject
                && crate::magician_v2::media_seam::meeting_bridge_linux::is_virtual_meeting_source(
                    name,
                )
            {
                // Leave must not write the bot's monitor back as the desktop mic.
                return Ok(());
            }
            // Held through the `set-default-source` so a leave cannot restore
            // the hardware mic in between, and a new join cannot flip it back
            // while leave is restoring.
            let _routing = if inject {
                Some(
                    crate::magician_v2::media_seam::meeting_bridge_linux::capture_routing_lock()
                        .await,
                )
            } else {
                None
            };
            let source = if inject {
                if let Some(current) = current_default_input().await {
                    crate::magician_v2::media_seam::meeting_bridge_linux::note_host_source(
                        &current,
                    );
                }
                crate::magician_v2::media_seam::meeting_bridge_linux::ensure_meeting_devices()
                    .await?;
                crate::magician_v2::media_seam::meeting_bridge_linux::mic_monitor()
            } else {
                name.to_string()
            };
            let bin = std::env::var("MEET_BOT_PACTL_BIN").unwrap_or_else(|_| "pactl".into());
            let out = tokio::process::Command::new(&bin)
                .args(["set-default-source", &source])
                .output()
                .await
                .map_err(|e| {
                    format!("{bin} is not runnable ({e}). Install Pulse tools with `make setup-meet-bot`.")
                })?;
            if !out.status.success() {
                return Err(format!(
                    "{bin} failed to set default source `{source}`: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            if inject {
                crate::magician_v2::media_seam::meeting_bridge_linux::acquire_meeting_audio_hold();
            }
            Ok(())
        }
    }

    /// Start Xvfb when the process has no display, and put `DISPLAY` on the
    /// browser engine env so the headed Meet window has somewhere to land.
    #[cfg(target_os = "linux")]
    async fn apply_virtual_display(
        engine_plan: &mut crate::magician_v2::execution::primitive_dispatch::browser::session::BrowserEnginePlan,
    ) -> Result<(), String> {
        if std::env::var_os("DISPLAY").is_some() {
            return Ok(());
        }
        let display = std::env::var("MEET_BOT_DISPLAY").unwrap_or_else(|_| ":99".to_string());
        let bin = std::env::var("MEET_BOT_XVFB_BIN").unwrap_or_else(|_| "Xvfb".to_string());
        let mut child = std::process::Command::new(&bin)
            .args([&display, "-screen", "0", "1280x720x24"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| {
                format!(
                    "{bin} is not runnable ({e}). Install Xvfb with `make setup-meet-bot`, \
                     or set DISPLAY."
                )
            })?;
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        if let Ok(Some(status)) = child.try_wait() {
            if !status.success() {
                info!(
                    %display,
                    "meet-join: Xvfb exited; assuming the display is already up"
                );
            }
        } else {
            // Keep the server for the life of this process. Dropping the child
            // would not kill it, but we also must not wait on it.
            std::mem::forget(child);
        }
        for engine in std::iter::once(&mut engine_plan.primary)
            .chain(engine_plan.capacity_fallback.iter_mut())
        {
            engine
                .env
                .entry("DISPLAY".to_string())
                .or_insert_with(|| display.clone());
        }
        Ok(())
    }

    /// Find the Chromium PID whose full argv contains `marker` (the unique
    /// `--user-data-dir`). `pgrep -f -- <marker>` matches against the whole
    /// command line; the first PID is taken.
    async fn pid_for_user_data_dir(marker: &str) -> Option<i32> {
        let out = tokio::process::Command::new("pgrep")
            .args(["-f", "--", marker])
            .output()
            .await
            .ok()?;
        parse_first_pid(&String::from_utf8_lossy(&out.stdout))
    }

    #[async_trait]
    impl BrowserJoin for AgentBrowserMeetJoiner {
        async fn join(&self, meet_url: &str, display_name: &str) -> Result<JoinedMeeting, String> {
            // 1. Save + flip the mic. The Meet tab's mic reads from the system
            //    default input, so we point it at BlackHole-16ch (the same device
            //    the inject sink writes to). Restored in leave().
            let prior = current_default_input().await;
            #[cfg(target_os = "linux")]
            let prior = {
                let resolved =
                    crate::magician_v2::media_seam::meeting_bridge_linux::host_source_to_restore(
                        prior.as_deref(),
                    )
                    .await
                    .ok()
                    .flatten();
                let prior = resolved.or(prior.filter(|name| {
                    !crate::magician_v2::media_seam::meeting_bridge_linux::is_virtual_meeting_source(
                        name,
                    )
                }));
                if let Some(ref name) = prior {
                    crate::magician_v2::media_seam::meeting_bridge_linux::note_host_source(name);
                }
                prior
            };
            info!(
                prior_input = ?prior,
                target_input = DEFAULT_INJECT_DEVICE,
                "meet-join: flipping default input device"
            );
            *self.prior_input.lock().await = prior;
            if let Err(e) = set_default_input(DEFAULT_INJECT_DEVICE).await {
                // The flip never happened — clear the saved device so a later
                // leave() doesn't "restore" stale state.
                self.prior_input.lock().await.take();
                return Err(e);
            }

            // 1b. Sanitize the (persistent) profile before launch so Chrome opens
            //     STRAIGHT to the Meet URL instead of restoring the previous session (a
            //     stale gmail/Meet tab) — the heavy restored page starves agent-browser's
            //     CDP connect and it times out. The bot kills the browser on leave, so at
            //     the NEXT startup Chrome re-marks the profile `Crashed` (before we can
            //     stop it) and crash-recovery restores the old tabs. `exit_type` edits and
            //     `--hide-crash-restore-bubble` don't stop that, so the ONLY reliable fix
            //     is to DELETE the tab-restore cache — crash-recovery then has nothing to
            //     reopen. We also clear the crash flag + force NTP-on-startup as belt-and-
            //     suspenders. Transient state only; the sign-in lives in Cookies/Login
            //     Data, which we never touch. A fresh temp profile has no Default/ yet —
            //     all steps are best-effort.
            {
                // tokio::fs throughout: this runs on the shared runtime that also
                // serves the HTTP API — blocking std::fs here would stall a worker.
                let default_dir = std::path::Path::new(&self.user_data_dir).join("Default");
                let prefs_path = default_dir.join("Preferences");
                if let Ok(raw) = tokio::fs::read_to_string(&prefs_path).await {
                    if let Ok(mut prefs) = serde_json::from_str::<serde_json::Value>(&raw) {
                        if let Some(obj) = prefs.as_object_mut() {
                            if let Some(p) = obj
                                .entry("profile")
                                .or_insert_with(|| serde_json::json!({}))
                                .as_object_mut()
                            {
                                p.insert("exit_type".into(), serde_json::json!("Normal"));
                                p.insert("exited_cleanly".into(), serde_json::json!(true));
                            }
                            if let Some(s) = obj
                                .entry("session")
                                .or_insert_with(|| serde_json::json!({}))
                                .as_object_mut()
                            {
                                // 5 = open the New Tab Page (do NOT restore last session).
                                s.insert("restore_on_startup".into(), serde_json::json!(5));
                            }
                        }
                        if let Ok(out) = serde_json::to_string(&prefs) {
                            // Atomic publish (what Chrome's own writer does for
                            // this file): an in-place rewrite that is
                            // interrupted leaves Chrome a half-written
                            // Preferences, which it treats as a broken profile
                            // and resets — reintroducing exactly the restore /
                            // first-run prompts this block exists to suppress.
                            // Still best-effort; a fresh profile has no
                            // Preferences and the launch proceeds either way.
                            let _ = write_bytes_durably(&prefs_path, out.as_bytes()).await;
                        }
                    }
                }
                // The decisive step: drop the tab-restore cache so crash-recovery has
                // nothing to reopen (Chrome restores from these regardless of exit_type
                // once it re-marks the profile Crashed at startup).
                for f in [
                    "Current Session",
                    "Current Tabs",
                    "Last Session",
                    "Last Tabs",
                ] {
                    let _ = tokio::fs::remove_file(default_dir.join(f)).await;
                }
                let _ = tokio::fs::remove_dir_all(default_dir.join("Sessions")).await;
                info!(
                    user_data_dir = %self.user_data_dir,
                    "meet-join: cleared profile crash/restore state before launch"
                );
            }

            // 2. Launch a dedicated headed browser on the Meet URL. Flags:
            //    --use-fake-ui-for-media-stream auto-accepts the mic/cam prompt;
            //    --hide-crash-restore-bubble + --no-first-run + --no-default-browser-check
            //    suppress the "Restore pages?" / first-run / default-browser prompts that
            //    otherwise block automated navigation after an unclean shutdown — the bot
            //    is killed/restarted often, which marks the PERSISTENT profile `Crashed`
            //    and would otherwise hang the join on the restore bubble (agent-browser
            //    then times out waiting for a clean page);
            //    --user-data-dir is the persistent profile that PID discovery keys off. A
            //    fresh session id keeps this browser isolated from any agent browsing.
            let session_id = format!("meet-join-{}", uuid::Uuid::new_v4());
            // Our own Chrome flags: media auto-accept + restore/first-run suppression.
            // CRITICAL: do NOT pass `--user-data-dir` / `--profile-directory` as raw
            // launch args. Overriding Chrome's user-data-dir desyncs agent-browser's
            // DevTools-port discovery (it reads `DevToolsActivePort` from the dir IT
            // manages, not the override) → the CDP connect hangs and `open` times out
            // with a blank window. The persistent signed-in profile is selected via the
            // `AGENT_BROWSER_PROFILE` env below: the PINNED agent-browser build treats a
            // path-valued profile as Chrome's literal --user-data-dir while keeping its
            // own port discovery in sync. The step-1b profile sanitize (edits
            // `<dir>/Default/*`) and `pid_for_user_data_dir`'s pgrep both DEPEND on the
            // path appearing verbatim as the launched Chrome's user-data-dir — revisit
            // them if an agent-browser bump changes path-profile handling.
            let meet_args = "--use-fake-ui-for-media-stream,\
                             --autoplay-policy=no-user-gesture-required,\
                             --hide-crash-restore-bubble,--no-first-run,--no-default-browser-check"
                .to_string();
            // Start from the configured browser-engine env so this launches the
            // same engine as the regular browser tool, then append the meeting
            // flags and point it at the persistent profile.
            let mut engine_plan = self.engine_plan.clone();
            for engine in std::iter::once(&mut engine_plan.primary)
                .chain(engine_plan.capacity_fallback.iter_mut())
            {
                let combined_args = match engine.env.get("AGENT_BROWSER_ARGS") {
                    Some(existing) if !existing.trim().is_empty() => {
                        format!("{existing},{meet_args}")
                    },
                    _ => meet_args.clone(),
                };
                engine
                    .env
                    .insert("AGENT_BROWSER_ARGS".to_string(), combined_args);
                engine.env.insert(
                    "AGENT_BROWSER_PROFILE".to_string(),
                    self.user_data_dir.clone(),
                );
            }
            #[cfg(target_os = "linux")]
            if let Err(error) = apply_virtual_display(&mut engine_plan).await {
                self.restore_prior_input().await;
                return Err(error);
            }
            info!(
                session_id = %session_id,
                user_data_dir = %self.user_data_dir,
                meet_url = %meet_url,
                engine = engine_plan
                    .primary
                    .env
                    .get("AGENT_BROWSER_EXECUTABLE_PATH")
                    .map(String::as_str)
                    .unwrap_or("default Chrome for Testing"),
                "meet-join: launching dedicated headed browser"
            );
            // Any failure from here on must restore the already-flipped mic — an
            // early `?` would strand the host's default input on BlackHole.
            let session = match AgentBrowserSession::new_with_session_id(
                session_id.clone(),
                ConnectionMode::Headed,
                self.cli_path.clone(),
            ) {
                Ok(s) => s
                    .with_initial_url(Some(meet_url.to_string()))
                    .with_engine_plan(engine_plan),
                Err(e) => {
                    self.restore_prior_input().await;
                    return Err(format!("build agent-browser session: {e}"));
                },
            };
            let session = match self.analytics.clone() {
                Some(analytics) => session.with_analytics_context(analytics),
                None => session,
            };

            if let Err(e) = session.ensure_connected().await {
                self.restore_prior_input().await;
                return Err(format!("agent-browser failed to launch/connect: {e}"));
            }

            // 3. Drive pre-join → admission as ONE tolerant, self-healing loop.
            //    Selectors are guesses (see the *_CANDIDATES consts) and are heavily
            //    logged so the live gate can correct them. Each poll classifies the
            //    page and acts:
            //      • in-call UI (a leave control) present → we're admitted, done. This
            //        is also the signed-in "Join now" fast path.
            //      • denied / meeting-gone text present → abort NOW (re-clicking a
            //        terminal page would re-knock and spam the host for the rest of
            //        the window, then mis-report "host did not admit").
            //      • "asking to be let in" / waiting text present → our knock already
            //        registered; keep waiting for the host, do NOT click again.
            //      • otherwise we're still on the pre-join page → (re)fill the name and
            //        (re)click the join control. Re-attempting self-heals the two
            //        common guest failures the old one-shot flow missed: the first
            //        snapshot landing before the pre-join page rendered, and "Ask to
            //        join" being disabled until a name is entered (so the first click
            //        was a no-op — the next poll re-clicks now that the name is filled).
            //    Bounded by WALL TIME, not iterations: a failing snapshot can burn
            //    minutes inside run_command's internal retry/relaunch chain, so an
            //    iteration budget would balloon the window from 2 minutes to hours
            //    (with the host's mic flipped the whole time).
            //    A guest's knock must be admitted by the host; signed-in members on the
            //    meeting's domain are admitted without a knock.
            let mut deadline = Instant::now() + Duration::from_secs(ADMIT_TIMEOUT_SECS);
            let mut admitted = false;
            // "A join click the CLI reported as landed has been issued." Gates ONLY
            // the waiting-page classification (waiting copy means 'knock registered'
            // only if we actually clicked). Deliberately NOT used to suppress the
            // name/click fallbacks — those run cooldown-paced on every pre-join poll,
            // so a phantom landed click (CLI success on a disabled control) can never
            // wedge self-healing.
            let mut clicked_join = false;
            let mut knock_logged = false;
            let mut prejoin_snapshot_logged = false;
            let mut last_click_attempt: Option<Instant> = None;
            let mut attempt: u32 = 0;
            while Instant::now() < deadline {
                let snapshot = match session.run_command(&["snapshot", "-i"]).await {
                    Ok(res) => res.stdout,
                    Err(e) => {
                        // Transient (page mid-navigation) — keep trying until deadline.
                        warn!(session_id = %session_id, attempt, error = %e, "meet-join: snapshot failed; retrying");
                        attempt += 1;
                        tokio::time::sleep(Duration::from_secs(ADMIT_POLL_INTERVAL_SECS)).await;
                        continue;
                    },
                };
                let lower = snapshot.to_lowercase();

                // Admitted? (in-call leave control present) — also the signed-in fast path.
                if LEAVE_BUTTON_CANDIDATES.iter().any(|c| lower.contains(c)) {
                    info!(session_id = %session_id, attempt, "meet-join: admitted (in-call UI detected)");
                    admitted = true;
                    break;
                }

                // Denied / meeting gone — terminal; abort instead of re-knocking.
                // STRUCTURAL guard against candidate-list false positives: a page
                // that still offers a join control ("Ask to join" / "Join now") is
                // by definition NOT terminal, no matter what stray text it carries
                // (the signed-in pre-join page has nav links like "Return to home
                // screen" that read terminal-ish).
                let joinable =
                    AgentBrowserMeetJoiner::find_ref(&snapshot, &["ask to join", "join now"])
                        .is_some();
                if !joinable
                    && PREJOIN_TERMINAL_TEXT_CANDIDATES
                        .iter()
                        .any(|c| lower.contains(c))
                {
                    error!(
                        session_id = %session_id,
                        attempt,
                        "meet-join: denied or meeting gone (terminal pre-admission page):\n{}",
                        snapshot.trim()
                    );
                    let _ = session.shutdown().await;
                    self.restore_prior_input().await;
                    return Err("join denied or meeting ended before admission".to_string());
                }

                // Knock registered — wait for the host, don't re-click. Gated on
                // `clicked_join`: waiting copy means a knock only if WE clicked;
                // a pre-join page whose helper text happens to match a waiting
                // candidate must still fall through and click, or no knock would
                // ever be sent.
                if clicked_join && WAITING_TEXT_CANDIDATES.iter().any(|c| lower.contains(c)) {
                    if !knock_logged {
                        info!(session_id = %session_id, attempt, "meet-join: knock registered — waiting for the host to admit");
                        knock_logged = true;
                        // The host's window starts at the KNOCK: slow pre-join
                        // driving (heavy page loads, snapshot retries) must not
                        // eat the admission budget.
                        deadline =
                            deadline.max(Instant::now() + Duration::from_secs(ADMIT_TIMEOUT_SECS));
                    }
                    attempt += 1;
                    tokio::time::sleep(Duration::from_secs(ADMIT_POLL_INTERVAL_SECS)).await;
                    continue;
                }

                // Still on the pre-join page → (re)fill name + (re)click join,
                // cooldown-paced. (`clicked_join` stays sticky: a click that landed
                // last poll may simply not have transitioned the page yet.) Log the
                // FULL snapshot the first time the page classifies pre-join (the
                // live-tuning data the selector guesses depend on), concise after.
                if !prejoin_snapshot_logged {
                    info!(session_id = %session_id, attempt, "meet-join: pre-join snapshot:\n{}", snapshot.trim());
                    prejoin_snapshot_logged = true;
                } else {
                    info!(session_id = %session_id, attempt, "meet-join: still on pre-join page; re-attempting name + join");
                }

                if let Some(name_ref) =
                    AgentBrowserMeetJoiner::find_ref(&snapshot, NAME_FIELD_CANDIDATES)
                {
                    info!(session_id = %session_id, name_ref = %name_ref, "meet-join: filling name field");
                    if let Err(e) = session
                        .run_command(&["fill", &name_ref, display_name])
                        .await
                    {
                        warn!(session_id = %session_id, error = %e, "meet-join: fill by ref failed; trying label locator");
                        let _ = session
                            .run_command(&["find", "label", "Your name", "fill", display_name])
                            .await;
                    }
                } else {
                    // Signed-in flows pre-fill/omit the name; try a semantic locator.
                    let _ = session
                        .run_command(&["find", "label", "Your name", "fill", display_name])
                        .await;
                }

                // (Re)click the join control, rate-limited by CLICK_RETRY_COOLDOWN —
                // re-clicking self-heals a click that didn't take (e.g. the button
                // was disabled until the name landed), but unthrottled it would
                // re-knock every poll. `clicked_join` records only clicks that
                // actually LANDED (ran + reported success).
                let click_allowed = last_click_attempt
                    .map(|t| t.elapsed() >= CLICK_RETRY_COOLDOWN)
                    .unwrap_or(true);
                if let Some(join_ref) =
                    AgentBrowserMeetJoiner::find_ref(&snapshot, JOIN_BUTTON_CANDIDATES)
                {
                    if click_allowed {
                        info!(session_id = %session_id, join_ref = %join_ref, "meet-join: clicking join control");
                        let ref_landed = match session.run_command(&["click", &join_ref]).await {
                            Ok(res) => res.success,
                            Err(e) => {
                                warn!(session_id = %session_id, error = %e, "meet-join: click by ref failed; trying text locators");
                                false
                            },
                        };
                        if ref_landed || click_join_by_text(&session, &session_id).await {
                            clicked_join = true;
                        }
                        last_click_attempt = Some(Instant::now());
                    }
                } else if click_allowed {
                    // No ref resolved → blind text fallback.
                    warn!(session_id = %session_id, attempt, "meet-join: no join button by ref; trying text locators \"Ask to join\"/\"Join now\"");
                    if click_join_by_text(&session, &session_id).await {
                        clicked_join = true;
                    }
                    last_click_attempt = Some(Instant::now());
                }

                attempt += 1;
                tokio::time::sleep(Duration::from_secs(ADMIT_POLL_INTERVAL_SECS)).await;
            }

            if !admitted {
                error!(
                    session_id = %session_id,
                    timeout_secs = ADMIT_TIMEOUT_SECS,
                    "meet-join: not admitted within timeout (never reached in-call UI)"
                );
                let _ = session.shutdown().await;
                self.restore_prior_input().await;
                return Err("host did not admit within timeout".to_string());
            }

            // 4. Discover the launched browser's PID for PID-targeted capture.
            let pid = pid_for_user_data_dir(&self.user_data_dir).await;
            if let Some(p) = pid {
                info!(session_id = %session_id, pid = p, "meet-join: resolved browser PID for capture");
            } else {
                // A live-gate red flag: capture will fall back to the bundle-id
                // default (whole-Chrome / whole-display) instead of THIS instance.
                warn!(
                    session_id = %session_id,
                    user_data_dir = %self.user_data_dir,
                    "meet-join: could NOT resolve a browser PID from the user-data-dir; \
                     capture will fall back to the bundle-id default (audio bleed risk)"
                );
            }

            #[cfg(target_os = "linux")]
            let capture_target = {
                if let Some(browser_pid) = pid {
                    match crate::magician_v2::media_seam::meeting_bridge_linux::route_sink_inputs_for_pid(
                        browser_pid,
                    )
                    .await
                    {
                        Ok(moved) if moved > 0 => {
                            info!(
                                session_id = %session_id,
                                pid = browser_pid,
                                moved,
                                "meet-join: browser output routed to the Pulse capture sink"
                            );
                            *self.routed_pid.lock().await = Some(browser_pid);
                            Some(CaptureTarget::Pid(browser_pid))
                        },
                        Ok(_) => {
                            info!(
                                session_id = %session_id,
                                pid = browser_pid,
                                "meet-join: browser has no Pulse sink-input yet; recording the default sink monitor"
                            );
                            Some(CaptureTarget::DisplayAudio)
                        },
                        Err(error) => {
                            warn!(
                                session_id = %session_id,
                                %error,
                                "meet-join: Pulse routing failed; recording the default sink monitor"
                            );
                            Some(CaptureTarget::DisplayAudio)
                        },
                    }
                } else {
                    Some(CaptureTarget::DisplayAudio)
                }
            };
            // Capture-target selection. SCK app-filtered audio is SILENT for the
            // cloak Chromium engine (live-proven 2026-06-10: a tone playing in
            // cloak measured 0.0 under an `org.chromium.Chromium` app filter while
            // whole-display capture heard it at full level — SCK never attributes
            // this engine's audio to its application; real Chrome attributed fine
            // in the original spike). So the DEFAULT is whole-display audio; the
            // session compensates by running half-duplex (capture is dropped while
            // the bot speaks, so it can't hear/transcribe/barge-in on itself).
            // Precedence:
            //   1. MEET_BOT_TARGET_BUNDLE_ID pin → app-filtered capture of that
            //      bundle (the operator escape hatch, wins outright);
            //   2. MEET_BOT_CAPTURE=bundle → app-filtered capture of the DERIVED
            //      bundle (engine exe → PID binary) for engines whose SCK
            //      attribution works — never the main PID itself (Chromium renders
            //      meeting audio in a helper process; a main-PID target is silent);
            //   3. default → whole-display audio (hears every app: notifications /
            //      music on the host bleed into the transcript — documented
            //      limitation until SCK attribution for the engine is solved).
            #[cfg(target_os = "macos")]
            let pinned = std::env::var("MEET_BOT_TARGET_BUNDLE_ID")
                .ok()
                .filter(|v| !v.trim().is_empty());
            #[cfg(target_os = "macos")]
            let prefer_bundle = std::env::var("MEET_BOT_CAPTURE")
                .map(|v| v.trim().eq_ignore_ascii_case("bundle"))
                .unwrap_or(false);
            #[cfg(target_os = "macos")]
            let capture_target = if let Some(bundle) = pinned {
                info!(session_id = %session_id, %bundle, "meet-join: capturing audio by PINNED app bundle id");
                Some(CaptureTarget::BundleId(bundle))
            } else if prefer_bundle {
                let mut capture_bundle =
                    match session.active_engine_env_value("AGENT_BROWSER_EXECUTABLE_PATH") {
                        Some(exe) => bundle_id_for_executable(&exe).await,
                        None => None,
                    };
                if capture_bundle.is_none() {
                    if let Some(p) = pid {
                        capture_bundle = bundle_id_for_pid(p).await;
                    }
                }
                match capture_bundle {
                    Some(bundle) => {
                        info!(session_id = %session_id, %bundle, "meet-join: capturing audio by derived app bundle id (MEET_BOT_CAPTURE=bundle)");
                        Some(CaptureTarget::BundleId(bundle))
                    },
                    None => {
                        warn!(
                            session_id = %session_id,
                            "meet-join: MEET_BOT_CAPTURE=bundle but no bundle id could be derived; \
                             falling back to whole-display audio (half-duplex)"
                        );
                        Some(CaptureTarget::DisplayAudio)
                    },
                }
            } else {
                info!(
                    session_id = %session_id,
                    "meet-join: capturing whole-display audio (app-filtered SCK capture is \
                     silent for this engine); session runs half-duplex"
                );
                Some(CaptureTarget::DisplayAudio)
            };
            *self.session.lock().await = Some(session);
            Ok(JoinedMeeting { capture_target })
        }

        async fn leave(&self) {
            // Best-effort: click the hang-up control, then close the browser. The
            // click matters: Meet drops the participant tile INSTANTLY on a real
            // hang-up, but a browser killed without one lingers as a ghost
            // attendee until Meet's dead-client timeout (minutes). The leave
            // control is an icon button (accessible NAME, not visible text), so
            // resolve it from the snapshot by ref first — a bare text locator
            // usually misses it — and verify the click actually landed.
            if let Some(session) = self.session.lock().await.take() {
                info!("meet-leave: clicking leave control + closing browser");
                let mut landed = false;
                if let Ok(res) = session.run_command(&["snapshot", "-i"]).await {
                    if let Some(leave_ref) =
                        AgentBrowserMeetJoiner::find_ref(&res.stdout, LEAVE_BUTTON_CANDIDATES)
                    {
                        landed = session
                            .run_command(&["click", &leave_ref])
                            .await
                            .map(|r| r.success)
                            .unwrap_or(false);
                    }
                }
                if !landed {
                    for label in ["Leave call", "Leave meeting"] {
                        landed = session
                            .run_command(&["find", "text", label, "click"])
                            .await
                            .map(|r| r.success)
                            .unwrap_or(false);
                        if landed {
                            break;
                        }
                    }
                }
                if !landed {
                    warn!(
                        "meet-leave: hang-up click did not land; closing the browser anyway \
                         (the Meet tile may linger until the dead-client timeout)"
                    );
                }
                let _ = session.shutdown().await;
            }
            // Restore the host's prior default input device.
            self.restore_prior_input().await;
        }

        async fn is_in_meeting(&self) -> bool {
            let guard = self.session.lock().await;
            let Some(session) = guard.as_ref() else {
                // No session → we never joined / already left. Not in a meeting.
                return false;
            };
            match session.run_command(&["snapshot", "-i"]).await {
                Ok(res) => {
                    let lower = res.stdout.to_lowercase();
                    if REMOVED_TEXT_CANDIDATES.iter().any(|c| lower.contains(c)) {
                        info!("meet-poll: removed/ended text detected; leaving");
                        return false;
                    }
                    // Still in the call only if the in-call leave UI is present.
                    let in_call = LEAVE_BUTTON_CANDIDATES.iter().any(|c| lower.contains(c));
                    if !in_call {
                        info!("meet-poll: in-call UI absent; treating as left");
                    }
                    in_call
                },
                Err(e) => {
                    // Transient snapshot failure — DON'T tear down on a blip.
                    warn!(error = %e, "meet-poll: snapshot failed; assuming still in meeting");
                    true
                },
            }
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use crate::magician_v2::media_seam::*;

    #[test]
    fn parse_first_pid_picks_first_of_multiple_lines() {
        assert_eq!(parse_first_pid("4321\n9999\n12345\n"), Some(4321));
    }

    #[test]
    fn parse_first_pid_trims_whitespace() {
        assert_eq!(parse_first_pid("  778  \n  900\n"), Some(778));
    }

    #[test]
    fn parse_first_pid_skips_leading_junk() {
        // pgrep should only emit numeric lines, but be tolerant of blanks/junk
        // and pick the first parseable PID.
        assert_eq!(parse_first_pid("\n\nnot-a-pid\n  \n55\n66\n"), Some(55));
    }

    #[test]
    fn parse_first_pid_empty_is_none() {
        assert_eq!(parse_first_pid(""), None);
    }

    #[test]
    fn parse_first_pid_only_junk_is_none() {
        assert_eq!(parse_first_pid("\n  \nfoo\nbar baz\n"), None);
    }

    #[tokio::test]
    async fn noop_join_returns_no_capture_target() {
        let joiner = NoopBrowserJoin;
        let joined = joiner
            .join("https://meet.google.com/abc-defg-hij", "Presto")
            .await
            .expect("noop join is infallible");
        assert!(joined.capture_target.is_none());
        // No-op never auto-tears-down.
        assert!(joiner.is_in_meeting().await);
        joiner.leave().await;
    }
}
