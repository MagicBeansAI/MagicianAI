//! Runnable Google-Meet bot (macOS and Linux) — a thin terminal front-end over the shared
//! `MeetingSessionManager`. It builds nothing itself: `manager.join()` constructs
//! the real session (capture → STT → summarize → respond-on-wake) from env, so
//! this runner and the agent-facing `meeting` tool share ONE code path.
//!
//! Prereqs (see docs/plans/2026-06-06-gmeet-participant-bot-macos.md):
//!   * `make build-macos-presence-host-debug` — stages ./magician-macos-meet-audio.bin
//!   * Screen Recording TCC grant (ScreenCaptureKit capture)
//!   * BlackHole 16ch installed + the desktop Meet's microphone set to it (inject)
//!   * the magician server running (:3002) — the agent's realtime voice (default)
//!   * OPENAI_API_KEY (cue TTS + the server's realtime rail); Ollama (summaries)
//!
//! Runtime selection:
//!   * Meeting STT/VAD/diarization resolve from the configured Meeting surface
//!     profile and scoped stage options.
//! Env overrides (read inside `MeetingSessionManager::join`):
//!   * MEET_BOT_RESPONDER       — orchestrator (default) | agent-tts | llm-tts | realtime-direct | noop
//!   * MEET_BOT_TARGET_BUNDLE_ID — which app's audio to capture (default Chrome)
//!   * MEET_BOT_TTS_VOICE       — voice for the short spoken cues
//!
//! Run (from the repo root):
//!   CARGO_TARGET_DIR=/Volumes/build/magician/builds \
//!     cargo run -p magician --example meet_bot -- "<meet-url-or-label>"
//!
//! NOTE: the bot does NOT auto-join the meeting yet (agent-browser join is a later
//! phase). Have the desktop Chrome already in the Meet; the bot captures Chrome's
//! audio via ScreenCaptureKit and speaks back through BlackHole 16ch. Say
//! "Hey Magican, …" to trigger a reply. Ctrl-C to leave.

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::main]
async fn main() {
    use magician::magician_v2::media_seam::meeting::{meeting_manager, MeetingConfig};

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let meet_url = std::env::args().nth(1).unwrap_or_default();
    let config = MeetingConfig {
        meet_url: meet_url.clone(),
        ..Default::default()
    };

    // Shared path: the manager builds the real session from env and spawns it.
    let manager = meeting_manager();
    let session_id = match manager
        .join(
            config,
            std::sync::Arc::new(
                magician::magician_v2::media_seam::meeting::NoopMeetingMemoryWriter,
            ),
            // Manual-join: this terminal harness rides an already-open Chrome in
            // the Meet (auto-join is exercised via the `meeting` tool / live gate).
            std::sync::Arc::new(magician::magician_v2::media_seam::meeting::NoopBrowserJoin),
            // No agent scope: chat and audio settings use the default scope.
            None,
            // No crash marker: terminal harness, nowhere to post a sweep note.
            None,
            // No realtime event broadcaster in this standalone terminal harness.
            None,
        )
        .await
    {
        Ok(id) => id,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(1);
        },
    };

    eprintln!(
        "meet-bot: joined as session {session_id}. \
         Say \"Hey Magican …\" to trigger a reply. Ctrl-C to leave."
    );

    let _ = tokio::signal::ctrl_c().await;
    eprintln!("meet-bot: leaving…");
    let _ = manager.leave(&session_id).await;
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn main() {
    eprintln!("the meet_bot example needs macOS or Linux.");
}
