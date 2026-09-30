//! Codex as a plane harness — the `codex app-server` surface.
//!
//! Unlike the `codex exec` engine (one process, prompt on argv, no
//! continuity), this variant speaks the app-server JSON-RPC protocol through
//! the shared `coding_engine` driver (`run_turn_spawned`): initialize →
//! thread/start or thread/resume → one turn → reap. Continuity comes from
//! Codex's own persisted threads, which live in `CODEX_HOME`: when the
//! request hands in a persistent home (`HarnessSessionRequest::native_home`,
//! the chat mouth's per-conversation one), each settled turn reports its
//! thread id as the native session id and the next session resumes it
//! (`resume_session_id`). A session with a temp home of its own reports no
//! id — the thread is deleted with the home, and `thread/resume` has no
//! fallback, so handing that id on would fail the caller's next turn.
//!
//! Governance parity with the exec engine: an isolated `CODEX_HOME` whose
//! `config.toml` points at the plane, grant via environment variable, never
//! argv. The app-server spawn runs under the coding-engine's OS sandbox and
//! env filter — shared with the vibedev path, so the disciplines cannot
//! drift. One child per turn is the established app-server pattern (the
//! adapter itself spawns per turn); thread resume, not a warm process, is
//! what carries state.
//!
//! The reply streams: each turn hands the driver a per-delta text tap
//! (`CodingEngineRequest::text_delta_sink`) that writes into the harness
//! sink as the app-server speaks, so the chat mouth shows the text live.
//! The driver's event sink is not that channel — it folds a run of message
//! deltas into one event for the cockpit stream.

use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::magician_v2::execution::coding_engine::codex::CodexAppServerAdapter;
use crate::magician_v2::execution::coding_engine::codex_contract::CodexLaunchProfile;
use crate::magician_v2::execution::coding_engine::factory::{CodexTurnMode, CodexTurnOptions};
use crate::magician_v2::execution::coding_engine::{
    CodingEngineRequest, CodingEngineTextDeltaSink,
};
use crate::magician_v2::execution::file_edit::transaction::TransactionScope;
use crate::magician_v2::execution::plane::engine::{
    HarnessCapabilities, HarnessEngine, HarnessError, HarnessSession, HarnessSessionRequest,
    HarnessStopReason, HarnessStreamSink, HarnessTurnInput, HarnessTurnSettled, HarnessUsage,
};
use crate::magician_v2::execution::plane::engines::codex::{plane_config_toml, GRANT_ENV_VAR};
use crate::magician_v2::execution::plane::engines::oneshot::{
    chmod_private_dir, operator_cli_home, require_cli_auth, seed_named_file, write_private,
};

const MAX_ASSISTANT_TEXT: usize = 16 * 1024;

#[derive(Debug, Clone)]
pub struct CodexAppServerEngine {
    /// Tests point this at a fake speaking the app-server protocol.
    pub binary: PathBuf,
}

impl Default for CodexAppServerEngine {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("codex"),
        }
    }
}

impl CodexAppServerEngine {
    /// The program the app-server child is spawned with. The adapter's spawn
    /// clears the child env and rebuilds it from an allowlist that re-sets
    /// `PATH`; a bare binary name combined with that forces std onto `fork`
    /// instead of `posix_spawn`, and a forked copy of this process can hang
    /// in macOS atfork handlers before exec (see `runtime_core::process`).
    /// The coding loop never hits this because discovery hands the adapter
    /// a canonical executable; the plane configures a name, so it resolves
    /// here against `path_env` — the process PATH (`None`) in production,
    /// which is what the allowlist copies through to the child.
    fn spawn_binary(&self, path_env: Option<&OsStr>) -> PathBuf {
        runtime_core::process::resolve_program(self.binary.as_os_str(), path_env)
    }
}

#[async_trait]
impl HarnessEngine for CodexAppServerEngine {
    fn name(&self) -> &'static str {
        "codex_app_server"
    }

    fn capabilities(&self) -> HarnessCapabilities {
        HarnessCapabilities {
            // Continuity is the persistent home plus `thread/resume`: a
            // caller that hands in a `native_home` gets the thread id back
            // and the next session resumes it inside the same CODEX_HOME. A
            // session on a temp home of its own reports no id (see the
            // module doc), so such a caller stays cold without a failed
            // resume.
            supports_resume: true,
            tools_list_changed: false,
            // The reply streams as the driver hears it: `turn` hands the
            // app-server driver a per-delta text tap that writes into the
            // harness sink (`delta_forwarder`), so the chat turn pumps
            // tokens live and does not re-send the settled text.
            streams_text_deltas: true,
            native_tool_posture:
                crate::magician_v2::execution::plane::engine::NativeToolPosture::Sandboxed,
        }
    }

    async fn start(
        &self,
        req: &HarnessSessionRequest,
    ) -> Result<Box<dyn HarnessSession>, HarnessError> {
        Ok(Box::new(CodexAppServerSession {
            adapter: CodexAppServerAdapter::new(self.spawn_binary(None)),
            endpoint_url: req.endpoint.url.clone(),
            grant: req.grant.clone(),
            model: req.model.clone(),
            planner_system: req.planning_only.then(|| req.system_prompt.clone()),
            cancel: req.cancel.clone(),
            turn_timeout: req.turn_timeout,
            resume_thread_id: req.resume_session_id.clone(),
            thread_confirmed: false,
            installed: false,
            home: None,
            native_home: req.native_home.clone(),
            owns_home: false,
            released: false,
        }))
    }
}

struct CodexAppServerSession {
    adapter: CodexAppServerAdapter,
    endpoint_url: String,
    grant: String,
    model: Option<String>,
    /// Carry the engine's proposal/reply rules, as the one-shot adapter does.
    planner_system: Option<String>,
    cancel: Option<tokio_util::sync::CancellationToken>,
    turn_timeout: std::time::Duration,
    resume_thread_id: Option<String>,
    /// Whether the driver confirmed `resume_thread_id` this session (a
    /// settled turn's continuation named it). A seeded id the CLI never
    /// resumed is not reported from a refused turn: re-storing it would
    /// refuse every later turn the same way.
    thread_confirmed: bool,
    installed: bool,
    /// The installed CODEX_HOME: `native_home` when the request handed one
    /// in, else this session's own temp dir.
    home: Option<std::path::PathBuf>,
    /// See `HarnessSessionRequest::native_home`.
    native_home: Option<std::path::PathBuf>,
    /// The home is the session's own temp dir, removed with it. A handed-in
    /// home belongs to the caller's continuation and is never removed here.
    owns_home: bool,
    released: bool,
}

impl CodexAppServerSession {
    /// Install the isolated CODEX_HOME with the plane MCP config — once per
    /// session, mirroring the exec engine's discipline. The config's grant
    /// channel is the environment variable, so the CLI's own rewrites of
    /// config.toml can never persist the token.
    fn install(&mut self) -> Result<(), HarnessError> {
        // Same home recipe as the one-shot base: the handed-in persistent
        // home when there is one, else a unique directory under the system
        // temp root, cleaned explicitly on release (no tempfile crate — the
        // magician crate does not depend on it). Config and auth below are
        // overwrite-safe, so a persistent home is re-installed per session.
        let owns_home = self.native_home.is_none();
        let home = self.native_home.clone().unwrap_or_else(|| {
            std::env::temp_dir().join(format!(
                "magician-plane-cax-{}",
                uuid::Uuid::new_v4().simple()
            ))
        });
        std::fs::create_dir_all(&home)
            .map_err(|error| HarnessError::Message(format!("codex home: {error}")))?;
        chmod_private_dir(&home);
        require_cli_auth("CODEX_HOME", ".codex", "OPENAI_API_KEY", "codex")
            .map_err(|error| HarnessError::Message(format!("codex auth: {error}")))?;
        seed_named_file(
            &operator_cli_home("CODEX_HOME", ".codex"),
            &home,
            "auth.json",
        )
        .map_err(|error| HarnessError::Message(format!("codex auth: {error}")))?;
        // The same isolated config as `codex exec` — one renderer, so the
        // two Codex postures cannot drift (read-only sandbox, shell and
        // unified_exec off, code-mode pair on, plane server pre-approved).
        let config = plane_config_toml(&self.endpoint_url);
        write_private(&home.join("config.toml"), config.as_bytes())
            .map_err(|error| HarnessError::Message(format!("codex config: {error}")))?;
        self.home = Some(home);
        self.owns_home = owns_home;
        self.installed = true;
        Ok(())
    }

    /// The thread id a caller may resume next session: only one that lives
    /// in a home outliving this session. On a temp home the thread goes
    /// with the directory, and `thread/resume` on a missing thread fails the
    /// turn rather than falling back to `thread/start`.
    fn resumable_thread_id(&self) -> Option<String> {
        if self.native_home.is_none() {
            return None;
        }
        self.resume_thread_id.clone()
    }

    /// `resumable_thread_id`, only once the driver has confirmed it this
    /// session: what a refused turn may report.
    fn confirmed_thread_id(&self) -> Option<String> {
        if !self.thread_confirmed {
            return None;
        }
        self.resumable_thread_id()
    }

    /// The driver request for one turn on the installed home. The reply
    /// reaches `sink` delta by delta through the driver's text tap while
    /// the turn runs; the settled `assistant_text` is the same text before
    /// its byte bound (the driver's, then this engine's cap), so a very
    /// long reply streams whole and settles cut.
    fn turn_request(
        &self,
        input: &HarnessTurnInput,
        sink: &HarnessStreamSink,
    ) -> Result<CodingEngineRequest, HarnessError> {
        let home_path = self
            .home
            .clone()
            .ok_or_else(|| HarnessError::Message("codex home missing".to_string()))?;
        let home = home_path.display().to_string();
        let prompt = match self
            .planner_system
            .as_deref()
            .filter(|text| !text.trim().is_empty())
        {
            Some(system) => format!("{system}\n\n{}", input.text),
            None => input.text.clone(),
        };
        let mut request = CodingEngineRequest::new(
            prompt,
            home_path.clone(),
            home_path.clone(),
            home_path.clone(),
            // The plane has no vibedev scope; the bookkeeping fields the
            // scope feeds are inert on this path (no termination key or
            // staged proposal; usage is captured by the execution seam). A labelled
            // pseudo-scope keeps plane runs out of member trees.
            TransactionScope {
                principal: "plane".to_string(),
                workspace: "plane".to_string(),
            },
        );
        request.codex = CodexTurnOptions {
            resume_thread_id: self.resume_thread_id.clone(),
            model: self.model.clone(),
            effort: None,
            // Discuss = read-only sandbox on thread/start and turn/start.
            // Native tools remain advertised; writes/shell escalate and
            // are blocked. Plane MCP is the governed hands.
            mode: CodexTurnMode::Discuss,
            // The spawn's argv would otherwise disable the code-mode pair
            // over the isolated config, and with it the plane's hands.
            launch_profile: CodexLaunchProfile::PlaneHarness,
        };
        request.working_dir = Some(home_path);
        request.env.insert("CODEX_HOME".to_string(), home);
        request
            .env
            .insert(GRANT_ENV_VAR.to_string(), self.grant.clone());
        request.stage_result = false;
        request.cancel_token = self.cancel.clone();
        if !self.turn_timeout.is_zero() {
            request.timeout = self.turn_timeout;
        }
        request.text_delta_sink = Some(delta_forwarder(sink));
        Ok(request)
    }

    async fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        crate::magician_v2::execution::plane::engine::revoke_session_grant(&self.grant).await;
        if let Some(home) = self.home.take() {
            if self.owns_home {
                let _ = std::fs::remove_dir_all(home);
            }
        }
    }
}

/// The driver's text tap, writing into the harness sink: one `emit` per
/// delta in the driver's order (the sink drops an empty delta itself). The
/// clone shares the chat turn's receiver and dies with the driver's turn,
/// so the turn's pump ends when the harness sink is dropped, as before.
fn delta_forwarder(sink: &HarnessStreamSink) -> CodingEngineTextDeltaSink {
    let sink = sink.clone();
    Arc::new(move |delta: &str| sink.emit(delta))
}

#[async_trait]
impl HarnessSession for CodexAppServerSession {
    async fn turn(
        &mut self,
        input: &HarnessTurnInput,
        sink: &HarnessStreamSink,
    ) -> Result<HarnessTurnSettled, HarnessError> {
        // A released session is terminal: its grant is revoked (and a home
        // of its own is gone) — a further turn could only spawn a child
        // with no governed path back to the plane.
        if self.released {
            return Ok(HarnessTurnSettled {
                assistant_text: String::new(),
                stop_reason: HarnessStopReason::Cancelled,
                usage: None,
                native_session_id: self.resumable_thread_id(),
            });
        }
        if self
            .cancel
            .as_ref()
            .is_some_and(|token| token.is_cancelled())
        {
            self.release().await;
            return Ok(HarnessTurnSettled {
                assistant_text: String::new(),
                stop_reason: HarnessStopReason::Cancelled,
                usage: None,
                native_session_id: self.resumable_thread_id(),
            });
        }
        if !self.installed {
            self.install()?;
        }

        let mut request = self.turn_request(input, sink)?;
        let usage_capture = std::sync::Arc::new(std::sync::Mutex::new(None));
        request.usage_capture = Some(usage_capture.clone());
        match self.adapter.run_turn_spawned(request).await {
            Ok(result) => {
                // The thread is this session's to resume within (a second
                // `turn` on the same home); whether the caller may is the
                // home's lifetime, decided below.
                if let Some(thread_id) = result
                    .continuation
                    .as_ref()
                    .map(|continuation| continuation.native_session_id.clone())
                    .filter(|thread_id| !thread_id.is_empty())
                {
                    self.resume_thread_id = Some(thread_id);
                    self.thread_confirmed = true;
                }
                let native_session_id = self.resumable_thread_id();
                let mut assistant_text = result.assistant_text.unwrap_or_default();
                if assistant_text.len() > MAX_ASSISTANT_TEXT {
                    // Floor to a char boundary: `String::truncate` panics on
                    // a mid-character cut, and 16 KiB can land anywhere in
                    // multibyte text.
                    let mut cut = MAX_ASSISTANT_TEXT;
                    while !assistant_text.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    assistant_text.truncate(cut);
                }
                Ok(HarnessTurnSettled {
                    assistant_text,
                    stop_reason: HarnessStopReason::Settled,
                    usage: usage_capture.lock().ok().and_then(|slot| {
                        // Codex's `inputTokens` already includes the cached
                        // part (`cachedInputTokens`), as OpenAI reports it.
                        slot.as_ref().map(|usage| HarnessUsage {
                            input_tokens: usage.input,
                            cached_input_tokens: usage.cache_read.min(usage.input),
                            cache_read_reported: true,
                            // The shared coding driver uses zero for an omitted
                            // cache-write bucket. Preserve positive reports; zero
                            // remains unknown until that driver tracks presence.
                            cache_creation_tokens: (usage.cache_write > 0)
                                .then_some(usage.cache_write),
                            cost_usd: usage
                                .cost_known
                                .then_some(usage.cost)
                                .filter(|cost| cost.is_finite() && *cost >= 0.0),
                            model: None,
                            output_tokens: usage.output,
                        })
                    }),
                    native_session_id,
                })
            },
            Err(error) => {
                // Match on the preserved error type, not its rendered text —
                // the driver's cancel path is `TurnFailed("cancelled")` and
                // its deadline is `Timeout`, and both are terminal-for-this-
                // session stops that must release, not warm refusals.
                use crate::magician_v2::execution::coding_engine::codex::CodexSessionError;
                if let Some(codex_error) = error.downcast_ref::<CodexSessionError>() {
                    let stop_reason = match codex_error {
                        CodexSessionError::Timeout => HarnessStopReason::TurnBudgetSpent,
                        CodexSessionError::TurnFailed(status) if status == "cancelled" => {
                            HarnessStopReason::Cancelled
                        },
                        _ => HarnessStopReason::Refused,
                    };
                    if !matches!(stop_reason, HarnessStopReason::Refused) {
                        self.release().await;
                        return Ok(HarnessTurnSettled {
                            assistant_text: String::new(),
                            stop_reason,
                            usage: None,
                            native_session_id: self.resumable_thread_id(),
                        });
                    }
                }
                // A refused turn keeps the session warm — the retry contract
                // the exec engines hold too — and reports its thread only if
                // the driver confirmed it this session: a seeded id that
                // failed to resume (a thread this home never held, a session
                // format the CLI no longer reads) must not be re-stored, or
                // every later turn refuses the same way.
                Ok(HarnessTurnSettled {
                    assistant_text: format!("codex app-server turn failed: {error}"),
                    stop_reason: HarnessStopReason::Refused,
                    usage: None,
                    native_session_id: self.confirmed_thread_id(),
                })
            },
        }
    }

    async fn shutdown(&mut self) {
        self.release().await;
    }
}

impl Drop for CodexAppServerSession {
    fn drop(&mut self) {
        // Sync filesystem cleanup only; the grant revoke needs a runtime
        // which may not exist here — the turn-engine's continuation revoke
        // covers a dropped-paused session, and grants expire regardless
        // (the one-shot base's documented contract). A persistent home is
        // the continuation's to remove.
        if let Some(home) = self.home.take() {
            if self.owns_home {
                let _ = std::fs::remove_dir_all(home);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn executable(dir: &std::path::Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, "#!/bin/sh\nexit 0\n").expect("script");
        let mut perms = fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("chmod");
        path
    }

    /// The configured name is bare and the adapter's spawn rebuilds the
    /// child env with `PATH`, so the binary handed to the adapter must be
    /// the absolute file found on the PATH the child receives.
    #[test]
    fn bare_binary_is_resolved_against_the_child_path_before_the_adapter_is_built() {
        let temp = tempfile::tempdir().expect("tempdir");
        let bare = "magician-plane-codex-spawn-probe";
        let expected = executable(temp.path(), bare);
        let engine = CodexAppServerEngine {
            binary: PathBuf::from(bare),
        };

        let resolved = engine.spawn_binary(Some(temp.path().as_os_str()));

        assert_eq!(resolved, expected);
        assert!(resolved.is_absolute());
    }

    /// A configured path (the fakes the protocol tests point at, or an
    /// operator's explicit binary) is handed to the adapter verbatim.
    #[test]
    fn configured_binary_path_reaches_the_adapter_verbatim() {
        let temp = tempfile::tempdir().expect("tempdir");
        let fake = temp.path().join("codex-fake");
        let engine = CodexAppServerEngine {
            binary: fake.clone(),
        };
        assert_eq!(engine.spawn_binary(None), fake);
        assert_eq!(engine.spawn_binary(Some(OsStr::new("/nonexistent"))), fake);
    }

    /// An installed session whose thread is known, without a spawn or the
    /// operator's Codex auth (which `install` checks).
    fn installed_session(
        home: PathBuf,
        native_home: Option<PathBuf>,
        owns_home: bool,
    ) -> CodexAppServerSession {
        CodexAppServerSession {
            adapter: CodexAppServerAdapter::new(PathBuf::from("codex-fake")),
            endpoint_url: "http://127.0.0.1:8899/mcp".to_string(),
            grant: "plt_test".to_string(),
            model: None,
            planner_system: None,
            cancel: None,
            turn_timeout: std::time::Duration::from_secs(1),
            resume_thread_id: Some("thread-1".to_string()),
            thread_confirmed: false,
            installed: true,
            home: Some(home),
            native_home,
            owns_home,
            released: false,
        }
    }

    #[tokio::test]
    async fn decision_planner_instructions_reach_app_server_turns() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = installed_session(temp.path().into(), None, true);
        let input = HarnessTurnInput {
            text: "Create a thread".into(),
            operator_steer: Vec::new(),
        };
        let sink = HarnessStreamSink::drain();
        assert_eq!(
            session.turn_request(&input, &sink).unwrap().prompt,
            input.text
        );
        session.planner_system = Some("Propose work; answer in text when finished.".into());
        assert_eq!(
            session.turn_request(&input, &sink).unwrap().prompt,
            "Propose work; answer in text when finished.\n\nCreate a thread"
        );
        session.released = true;
    }

    /// A handed-in home belongs to the conversation: release and Drop both
    /// leave it, with the threads Codex persisted in it, for the next
    /// session to resume into.
    #[tokio::test]
    async fn a_persistent_home_survives_release_and_drop() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("conversation-home");
        fs::create_dir_all(home.join("sessions")).expect("home");
        let mut session = installed_session(home.clone(), Some(home.clone()), false);

        session.release().await;
        assert!(home.join("sessions").is_dir(), "release left the home");
        drop(session);
        assert!(home.join("sessions").is_dir(), "Drop left the home");
    }

    /// The session's own temp home goes with it (the pre-parity shape).
    #[tokio::test]
    async fn a_temp_home_is_removed_on_release() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("magician-plane-cax-test");
        fs::create_dir_all(&home).expect("home");
        let mut session = installed_session(home.clone(), None, true);

        session.release().await;
        assert!(!home.exists(), "release removed the session's own home");
    }

    #[test]
    fn a_temp_home_is_removed_on_drop() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("magician-plane-cax-test");
        fs::create_dir_all(&home).expect("home");
        let session = installed_session(home.clone(), None, true);
        drop(session);
        assert!(!home.exists(), "Drop removed the session's own home");
    }

    /// A thread id is only worth handing on when the home holding it
    /// outlives the session: the driver's `thread/resume` has no fallback,
    /// so a caller on a temp home (the loop seam) must stay cold rather
    /// than resume into a deleted CODEX_HOME.
    #[tokio::test]
    async fn the_thread_id_is_reported_only_from_a_persistent_home() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("home");

        let persistent = installed_session(home.clone(), Some(home.clone()), false);
        assert_eq!(
            persistent.resumable_thread_id().as_deref(),
            Some("thread-1")
        );

        let ephemeral = installed_session(home.clone(), None, true);
        assert_eq!(ephemeral.resumable_thread_id(), None);

        // The released early-return in `turn` is the same rule (no spawn).
        let mut released = installed_session(home.clone(), Some(home.clone()), false);
        released.released = true;
        let settled = released
            .turn(
                &HarnessTurnInput {
                    text: "again".to_string(),
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            )
            .await
            .expect("a released turn settles");
        assert_eq!(settled.stop_reason, HarnessStopReason::Cancelled);
        assert_eq!(settled.native_session_id.as_deref(), Some("thread-1"));

        let mut released = installed_session(home, None, true);
        released.released = true;
        let settled = released
            .turn(
                &HarnessTurnInput {
                    text: "again".to_string(),
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            )
            .await
            .expect("a released turn settles");
        assert_eq!(settled.native_session_id, None);
    }

    #[test]
    fn the_engine_advertises_resume() {
        assert!(
            CodexAppServerEngine::default()
                .capabilities()
                .supports_resume
        );
    }

    /// What `streams_text_deltas` promises the chat turn: the request a
    /// turn hands the driver carries a text tap that writes each delta into
    /// the harness sink, in order, dropping the empty ones; and the tap is
    /// the sink's only other sender, so the turn's pump still ends when the
    /// harness sink is dropped.
    #[tokio::test]
    async fn app_server_forwards_text_deltas_to_the_sink() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("home");
        let session = installed_session(home.clone(), Some(home), false);
        let (sink, mut rx) = HarnessStreamSink::channel();

        let request = session
            .turn_request(
                &HarnessTurnInput {
                    text: "say hello".to_string(),
                    operator_steer: Vec::new(),
                },
                &sink,
            )
            .expect("request");
        assert!(
            CodexAppServerEngine::default()
                .capabilities()
                .streams_text_deltas,
            "the capability and the wiring are one promise"
        );
        let tap = request
            .text_delta_sink
            .clone()
            .expect("the driver is handed a text tap");
        tap("hel");
        tap("");
        tap("lo");
        drop(tap);
        drop(request);
        drop(sink);

        assert_eq!(rx.recv().await.as_deref(), Some("hel"));
        assert_eq!(rx.recv().await.as_deref(), Some("lo"));
        assert_eq!(rx.recv().await, None, "every sender dropped: the pump ends");
    }

    /// A refused turn reports the thread only once the driver confirmed it
    /// this session. A session whose id was only ever seeded from the
    /// request (the resume itself failed) reports none, so the caller drops
    /// it and the next turn runs cold instead of refusing forever.
    #[tokio::test]
    async fn a_refused_turn_reports_no_thread_that_was_only_seeded() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("home");

        let seeded = installed_session(home.clone(), Some(home.clone()), false);
        assert!(!seeded.thread_confirmed);
        assert_eq!(seeded.confirmed_thread_id(), None, "seeded, never resumed");
        assert_eq!(
            seeded.resumable_thread_id().as_deref(),
            Some("thread-1"),
            "the other stops still hand the seeded id back"
        );

        let mut confirmed = installed_session(home.clone(), Some(home.clone()), false);
        confirmed.thread_confirmed = true;
        assert_eq!(confirmed.confirmed_thread_id().as_deref(), Some("thread-1"));

        // Confirmation never outranks the home: a temp-home session reports
        // nothing either way.
        let mut ephemeral = installed_session(home.clone(), None, true);
        ephemeral.thread_confirmed = true;
        assert_eq!(ephemeral.confirmed_thread_id(), None);

        // Through `turn` itself: an empty prompt is refused by the driver's
        // request validation before any spawn, which lands in the same
        // Refused arm a failed `thread/resume` does.
        async fn refused_turn(session: &mut CodexAppServerSession) -> HarnessTurnSettled {
            session
                .turn(
                    &HarnessTurnInput {
                        text: String::new(),
                        operator_steer: Vec::new(),
                    },
                    &HarnessStreamSink::drain(),
                )
                .await
                .expect("a refused turn settles")
        }
        let mut seeded = installed_session(home.clone(), Some(home.clone()), false);
        let settled = refused_turn(&mut seeded).await;
        assert_eq!(settled.stop_reason, HarnessStopReason::Refused);
        assert_eq!(settled.native_session_id, None, "seeded only: dropped");
        assert!(!seeded.released, "a refused turn keeps the session warm");

        let mut confirmed = installed_session(home.clone(), Some(home), false);
        confirmed.thread_confirmed = true;
        let settled = refused_turn(&mut confirmed).await;
        assert_eq!(settled.stop_reason, HarnessStopReason::Refused);
        assert_eq!(
            settled.native_session_id.as_deref(),
            Some("thread-1"),
            "confirmed this session: kept"
        );
    }
}
