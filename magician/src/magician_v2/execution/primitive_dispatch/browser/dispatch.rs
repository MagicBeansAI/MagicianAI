//! Thin bridge from inner-loop browser tool calls to `agent-browser` argv.
//!
//! The browser pack exposes the CLI surface directly through exact argv tools:
//! each YAML-declared command receives `args` that become the tokens after the
//! command name. Rust executes those argv tokens through the pinned
//! `agent-browser` binary and intentionally does not maintain a semantic
//! browser API or command-specific lowering layer.

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::owned_tabs_header::render_owned_tabs_header;
use super::session::{
    AgentBrowserSession, AgentBrowserToolResult, DEFAULT_COMMAND_TIMEOUT_SECS,
    LIGHTPANDA_BROWSER_ENGINE_NAME,
};
use super::yutori_translator::{
    is_yutori_action, translate_yutori_action, yutori_action_might_press_mouse,
};
use crate::magician_v2::execution::primitive_dispatch::exec_ctx::{
    CAPTURE_WITHHELD_NONE, CAPTURE_WITHHELD_PAGE, CAPTURE_WITHHELD_PIXELS,
};
use crate::magician_v2::execution::primitive_dispatch::runner::{
    PrimitiveDispatcher, PrimitiveToolResult,
};
use crate::magician_v2::execution::primitive_dispatch::PrimitiveArtifact;
use crate::magician_v2::json_traversal::map_json_strings_owned;
use crate::magician_v2::secrets::{
    known_value_replacements, resolve_inline_placeholders, KnownSecretValues, SecretStore,
};

const AGENT_BROWSER_SCREENSHOT_DIR_ENV: &str = "AGENT_BROWSER_SCREENSHOT_DIR";

/// Inject a small wait between consecutive subcommands of a structured
/// `batch.commands` invocation before forwarding to agent-browser. The
/// CLI dispatches batch subcommands back-to-back with no gap, which can
/// drop intermediate pointer events on rapid mouse-drag sequences and
/// leave drag handlers in a half-applied state. A short wait between
/// each pair gives the page's pointer/event handlers time to fire and
/// settle. Default 50ms; override via the env var below. Set to 0 to
/// disable entirely.
const DEFAULT_BATCH_COMMAND_SPACING_MS: u64 = 50;
const BATCH_COMMAND_SPACING_ENV: &str = "MAGICIAN_AGENT_BROWSER_BATCH_SPACING_MS";
/// Extra settle delay (ms) inserted AFTER a high-level mutating subcommand
/// (click/drag/fill/type/press/check/uncheck/select/tap/upload) before the next
/// subcommand — giving the page time to run its handlers and re-render so the
/// next step sees the settled result. Raw `mouse` micro-steps (move/down/up) are
/// fragments of a single gesture and keep the base spacing, not the settle.
const DEFAULT_BATCH_MUTATION_SETTLE_MS: u64 = 150;
const BATCH_MUTATION_SETTLE_ENV: &str = "MAGICIAN_AGENT_BROWSER_MUTATION_SETTLE_MS";

/// Browser implementation of the generic inner-loop dispatcher.
///
/// Executes exact `agent-browser` argv selected by the inner LLM, preserving the
/// shared browser session and projecting stdout/stderr/JSON/artifacts into the
/// generic runner result shape.
#[derive(Clone)]
pub struct BrowserDispatcher {
    session: Arc<AgentBrowserSession>,
    artifact_root: Option<PathBuf>,
    command_timeout_secs: u64,
    secret_store: Option<Arc<SecretStore>>,
    ephemeral_secret_scope_id: Option<String>,
    /// The run's delivered-values set and capture-withheld level (P4). See
    /// `PrimitiveExecCtx::with_delivery_tracking`.
    delivered_secret_values: Option<Arc<std::sync::Mutex<KnownSecretValues>>>,
    browser_capture_withheld: Option<Arc<std::sync::atomic::AtomicU8>>,
    /// Enable the Yutori Navigator N1.5 action-translation shim. Only true for
    /// Yutori-driven runs (resolved decision provider == Yutori). Yutori emits
    /// coordinate-based `left_click`/`drag`/`scroll` vocab that must be
    /// translated to agent-browser mouse argv; every other provider uses native
    /// agent-browser commands directly, so the shim stays off and avoids
    /// hijacking native command names like `drag`/`scroll`.
    yutori_translation: bool,
    /// Where a submission is sent for the owner's approval. Without one, a
    /// submission is refused outright.
    submission_approver: Option<SubmissionApprover>,
}

/// How a held submission was settled.
enum SubmissionAnswer {
    Approved(&'static str),
    Declined,
    Unavailable,
}

/// The owner-approval channel for form submissions in this run.
#[derive(Clone)]
pub struct SubmissionApprover {
    service: Arc<crate::magician_v2::user_requests::UserRequestService>,
    asker: super::submit_guard::ApprovalAsker,
    /// Where this scope's "Approve Always" grants are kept.
    grants_path: PathBuf,
}

impl SubmissionApprover {
    pub fn new(
        service: Arc<crate::magician_v2::user_requests::UserRequestService>,
        principal: String,
        workspace: String,
        execution_id: Option<String>,
        task_id: Option<String>,
        agent_id: Option<String>,
        grants_path: PathBuf,
    ) -> Self {
        Self {
            service,
            asker: super::submit_guard::ApprovalAsker {
                principal,
                workspace,
                execution_id,
                task_id,
                agent_id,
            },
            grants_path,
        }
    }
}

impl BrowserDispatcher {
    pub fn new(session: Arc<AgentBrowserSession>) -> Self {
        Self::with_artifact_root(session, None)
    }

    pub fn with_artifact_root(
        session: Arc<AgentBrowserSession>,
        artifact_root: Option<PathBuf>,
    ) -> Self {
        Self::with_options(session, artifact_root, DEFAULT_COMMAND_TIMEOUT_SECS)
    }

    pub fn with_options(
        session: Arc<AgentBrowserSession>,
        artifact_root: Option<PathBuf>,
        command_timeout_secs: u64,
    ) -> Self {
        Self {
            session,
            artifact_root,
            command_timeout_secs: command_timeout_secs.max(1),
            secret_store: None,
            ephemeral_secret_scope_id: None,
            delivered_secret_values: None,
            browser_capture_withheld: None,
            yutori_translation: false,
            submission_approver: None,
        }
    }

    /// Attach the run's delivered-values set and capture-withheld level (P4).
    pub fn with_delivery_tracking(
        mut self,
        delivered: Option<Arc<std::sync::Mutex<KnownSecretValues>>>,
        capture_withheld: Option<Arc<std::sync::atomic::AtomicU8>>,
    ) -> Self {
        self.delivered_secret_values = delivered;
        self.browser_capture_withheld = capture_withheld;
        self
    }

    pub fn with_secret_context(
        mut self,
        store: Option<Arc<SecretStore>>,
        ephemeral_scope_id: Option<String>,
    ) -> Self {
        self.secret_store = store;
        self.ephemeral_secret_scope_id = ephemeral_scope_id.and_then(|value| {
            let trimmed = value.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        });
        self
    }

    /// Enable the Yutori N1.5 action-translation shim for this dispatch. Off by
    /// default; the caller flips it on only when the decision provider is Yutori.
    pub fn with_yutori_translation(mut self, enabled: bool) -> Self {
        self.yutori_translation = enabled;
        self
    }

    /// Route form submissions to the owner for approval instead of refusing
    /// them. Without this the guard stays a hard refusal.
    pub fn with_submission_approver(mut self, approver: Option<SubmissionApprover>) -> Self {
        self.submission_approver = approver;
        self
    }
}

#[async_trait]
impl PrimitiveDispatcher for BrowserDispatcher {
    /// Browser-pack iteration header — surfaces the per-execution
    /// owned-tab inventory (Tier 1 + Tier 2) so the model sees what
    /// tabs exist, which one is current, and whether a popup it
    /// should engage with has appeared mid-flow. See
    /// `owned_tabs_header::render_owned_tabs_header` for the rendering
    /// rules and the staged revert path documented in
    /// `docs/plans/2026-05-03-browser-pack-target-ownership-and-tabs.md`.
    async fn iteration_header(&self) -> Option<String> {
        render_owned_tabs_header(&self.session).await
    }

    /// Browser-pack viewport in CSS pixels. Reported to the inner-loop
    /// runner so vision-cohort providers (Yutori N1) can denormalize the
    /// model's 1000×1000 coordinate output to actual viewport pixels
    /// before commands like `mouse move <x> <y>` reach agent-browser.
    ///
    /// Queries the live session via `agent-browser eval` on each call so
    /// user-driven resize, devtools docking, or programmatic
    /// `set viewport` changes are honored. Falls back to the
    /// agent-browser default `(1280, 800)` only when the eval fails or
    /// returns an unparseable shape — which keeps the SoTA test suite
    /// (where the viewport is always the CLI default) and any non-Chrome
    /// engines working transparently.
    async fn viewport(&self) -> Option<(u32, u32)> {
        match self.query_live_viewport().await {
            Some(dims) => Some(dims),
            None => Some((1280, 800)),
        }
    }

    /// Yutori N1.5 emits its native `browser_tools_core` action
    /// vocabulary regardless of the catalog we send. The inner-loop
    /// runner gates dispatch on a `tool_index` built from the
    /// catalog, so without registering these names a Yutori-emitted
    /// `mouse_move` / `left_click` / `goto_url` / etc. fails fast
    /// at the gate with "unknown inner-loop tool" — the dispatcher
    /// (and our translator) never runs. Expose them here so the
    /// runner registers each as a non-terminal tool that flows to
    /// `dispatch()`, where `is_yutori_action` + `translate_yutori_action`
    /// take over.
    fn extra_tool_names(&self) -> Vec<String> {
        // Mirror the recognition predicate in
        // `yutori_translator::is_yutori_action`. Kept as an explicit
        // list rather than calling the predicate so the surface area
        // is reviewable in one place. browser_tools_core +
        // expanded — expanded names get a clean "not supported"
        // error from the translator instead of falling through to
        // the agent-browser native dispatcher.
        vec![
            // click variants
            "left_click",
            "right_click",
            "middle_click",
            "double_click",
            "triple_click",
            // pointer / drag
            "mouse_move",
            "mouse_down",
            "mouse_up",
            "drag",
            // scroll / input
            "scroll",
            "type",
            "key_press",
            "hold_key",
            "wait",
            // navigation
            "goto_url",
            "go_back",
            "go_forward",
            "refresh",
            // expanded set — recognised so the translator can emit
            // a clear "not supported" message rather than letting
            // the runner reject them at the catalog gate.
            "extract_elements",
            "find",
            "set_element_value",
            "execute_js",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }

    async fn dispatch(&self, tool_name: &str, arguments: &Value) -> Result<PrimitiveToolResult> {
        if tool_name == "help" {
            let args = help_tool_call_to_cli_args(arguments)?;
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let result = self.session.run_cli_command(&arg_refs).await?;
            return Ok(PrimitiveToolResult {
                success: result.success,
                stdout: result.stdout,
                stderr: result.stderr,
                parsed_json: result.parsed_json,
                artifacts: Vec::new(),
                elapsed_ms: result.elapsed_ms,
            });
        }

        // Yutori Navigator N1.5 emits its native browser-action
        // vocabulary (`left_click`, `drag`, `type`, `scroll`,
        // `goto_url`, …) regardless of what `tools` we pass. Recognise
        // those names here and translate them to one or more
        // `agent-browser` commands — multi-step expansions ride
        // through `agent-browser batch --bail` so a single Yutori
        // action stays one subprocess. The existing native-name path
        // below is unchanged for non-N1.5 models and for any inner-
        // loop tool that uses our own catalog directly. The
        // translator no longer needs the viewport at dispatch time
        // (the N1.5 reference SDK uses a flat `amount × 100` CSS-pixel
        // scroll, so we follow that — the runtime authority over the
        // docs' "≈10 % screen height" wording).
        // Yutori Navigator N1.5 emits its native browser-action vocabulary
        // (`left_click`, `drag`, `scroll`, `goto_url`, …) regardless of the tools
        // we pass, with coordinate-based args. Translate those to agent-browser
        // mouse argv — but ONLY when the decision provider is actually Yutori
        // (`yutori_translation`). For every other provider this shim would hijack
        // native command names like `drag`/`scroll` and demand coordinates the
        // model never sends (it passes selectors per the native schema, which is
        // correct), so non-Yutori runs go straight to agent-browser native argv.
        let yutori = self.yutori_translation && is_yutori_action(tool_name);
        let invocation = if yutori {
            let invocation = translate_yutori_action(tool_name, arguments)?;
            local_access_check(&invocation.artifact_commands)?;
            invocation
        } else {
            tool_call_to_cli_invocation(tool_name, arguments)?
        };
        if self.session.active_engine_name().as_deref() == Some(LIGHTPANDA_BROWSER_ENGINE_NAME) {
            if let Some(command) = lightpanda_render_only_command(&invocation.artifact_commands) {
                bail!(
                    "browser command `{command}` requires rendered pixels, but the active \
                     Lightpanda session is DOM-only. Start the task with the configured \
                     full-fidelity engine (or explicit `engine: cloak-browser`) when screenshots, \
                     PDFs, recording, or vision evidence may be required"
                );
            }
        }
        // A form submission is an outward act the browser has no bindable
        // form for. The guard that holds it for the owner is an explicit
        // opt-in (`outward_actions.browser_submission_guard`); off, nothing
        // here runs and no DOM probe is spent. See `submit_guard` for what
        // counts and what the probe cannot see.
        if crate::magician_v2::agents::outward_actions::browser_submission_guard_enabled() {
            if let Some(reason) = self.submission_refusal(&invocation).await {
                match self.confirm_submission(&reason, &invocation).await {
                    SubmissionAnswer::Approved(how) => tracing::info!(
                        tool = %tool_name,
                        reason = %reason,
                        approval = how,
                        "[OUTWARD-APPROVED] browser submission approved"
                    ),
                    SubmissionAnswer::Declined => {
                        tracing::warn!(
                            tool = %tool_name,
                            reason = %reason,
                            "[OUTWARD-DECLINED] the owner did not approve this browser submission"
                        );
                        return Ok(submission_declined_result(&reason));
                    },
                    SubmissionAnswer::Unavailable => {
                        tracing::warn!(
                            tool = %tool_name,
                            reason = %reason,
                            "[OUTWARD-REFUSED] browser form submission refused; no owner to ask"
                        );
                        return Ok(submission_refused_result(&reason));
                    },
                }
            }
        }
        let artifact_root_before = self.artifact_root_snapshot().await?;
        let (invocation, known_secret_values) = self.resolve_invocation_secrets(invocation)?;
        // A secret was just typed into this page (P4): pixels cannot be
        // redacted, so an image capture is withheld until the page moves on.
        // Text observations still run and are scrubbed below — unless the
        // value was split across fields, where no scrub can recognise it and
        // only a command that acts without reading the page may run.
        let withheld_level = self
            .browser_capture_withheld
            .as_ref()
            .map(|withheld| withheld.load(std::sync::atomic::Ordering::Acquire))
            .unwrap_or(CAPTURE_WITHHELD_NONE);
        if withheld_level >= CAPTURE_WITHHELD_PAGE && !invocation_only_acts(&invocation) {
            return Ok(withheld_page_result());
        }
        if withheld_level >= CAPTURE_WITHHELD_PIXELS && invocation_captures_pixels(&invocation) {
            return Ok(withheld_capture_result());
        }
        let mut result = match self.run_command(&invocation).await {
            Ok(result) => result,
            Err(err) => {
                let sanitized =
                    sanitize_text_with_known_values(&err.to_string(), &known_secret_values);
                return Err(anyhow!(self.scrub_with_delivered(&sanitized)));
            },
        };
        // The withholding ends when the page has demonstrably moved on: a
        // navigation, or — when the value was delivered whole — a text
        // observation of the page that no longer contains it. A click or a
        // key press proves nothing (a "show password" toggle is a click), so
        // it clears nothing; a split value is recognisable in no text, so
        // only a navigation clears that level.
        if withheld_level > CAPTURE_WITHHELD_NONE {
            if let Some(withheld) = self.browser_capture_withheld.as_ref() {
                let page_moved = invocation_moves_the_page(&invocation)
                    || (withheld_level < CAPTURE_WITHHELD_PAGE
                        && invocation_observes_page_text(&invocation)
                        && !self.text_mentions_delivered_value(&result.stdout));
                if page_moved {
                    withheld.store(CAPTURE_WITHHELD_NONE, std::sync::atomic::Ordering::Release);
                }
            }
        }
        sanitize_agent_browser_result(&mut result, &known_secret_values);
        self.scrub_result_with_delivered(&mut result);
        reject_truncated_capture(&mut result);
        // A model-authored eval result is not opened-page evidence. Capture
        // the current page independently with a host-owned read, retaining
        // the original CLI response and existing capture/secret controls.
        let capture_allowed = self.browser_capture_withheld.as_ref().is_none_or(|level| {
            level.load(std::sync::atomic::Ordering::Acquire) == CAPTURE_WITHHELD_NONE
        });
        if invocation_reads_page(&invocation) {
            let capture_started = std::time::Instant::now();
            let evidence = if result.success && capture_allowed {
                self.capture_current_page_evidence().await
            } else {
                None
            };
            result.elapsed_ms = result
                .elapsed_ms
                .saturating_add(capture_started.elapsed().as_millis().min(u64::MAX as u128) as u64);
            attach_host_page_evidence(&mut result, evidence);
            sanitize_agent_browser_result(&mut result, &known_secret_values);
            self.scrub_result_with_delivered(&mut result);
            // Direct browser dispatch and legacy artifact readers consume
            // stdout. Keep it aligned with the authoritative structured lane.
            result.stdout = result
                .parsed_json
                .as_ref()
                .expect("object checked above")
                .to_string();
        }
        // Defensive mouse-state cleanup: Yutori actions that press a button
        // (left_click, right_click, middle_click, drag, mouse_down) expand to a
        // multi-step batch where `mouse down` happens before `mouse up`. If
        // `--bail` aborts mid-batch (CDP error, target closed, etc.), the up
        // never fires and the cursor is stuck pressed for the next iteration.
        // Fire a best-effort `mouse up` for every button — idempotent in CDP.
        // Only runs on the Yutori failure path. (Round-4 audit S1.)
        if yutori && !result.success && yutori_action_might_press_mouse(tool_name) {
            for button in ["", "right", "middle"] {
                let mut argv = vec!["mouse", "up"];
                if !button.is_empty() {
                    argv.push(button);
                }
                // Session-scoped: `run_command` keeps the mouse-up on the active
                // browser session (`run_cli_command` would hit `default`).
                let _ = self.session.run_command(&argv).await;
            }
        }
        let mut artifacts = artifacts_for_commands(
            &invocation.artifact_commands,
            &result.stdout,
            result.parsed_json.as_ref(),
        );
        artifacts.extend(self.artifacts_created_since(artifact_root_before).await?);
        dedupe_artifacts_by_path(&mut artifacts);
        Ok(PrimitiveToolResult {
            success: result.success,
            stdout: result.stdout,
            stderr: result.stderr,
            parsed_json: result.parsed_json,
            artifacts,
            elapsed_ms: result.elapsed_ms,
        })
    }
}

fn lightpanda_render_only_command(commands: &[Vec<String>]) -> Option<&str> {
    commands
        .iter()
        .filter_map(|command| command.first().map(String::as_str))
        .find(|command| matches!(*command, "screenshot" | "pdf" | "record"))
}

fn reject_truncated_capture(result: &mut AgentBrowserToolResult) {
    if !result.stdout_truncated && !result.stderr_truncated {
        return;
    }
    result.success = false;
    result.stdout.clear();
    result.parsed_json = None;
    result.stderr = "browser command output exceeded its configured capture limit".into();
}

impl BrowserDispatcher {
    async fn capture_current_page_evidence(&self) -> Option<Value> {
        // Bound capture in the browser before crossing the process boundary.
        // For a batch this attests only the final page, never earlier results.
        const SCRIPT: &str = "JSON.stringify((()=>{const t=document.body?.innerText||'';return {url:location.href,title:document.title,text:t.slice(0,12000),truncated:t.length>12000}})())";
        let capture = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            self.session
                .run_command_with_options_and_stdin(&["eval", SCRIPT], 3, &[], None),
        )
        .await
        .ok()?
        .ok()?;
        browser_page_evidence(&capture)
    }

    /// Exact live viewport for physical-owner attestations. Unlike the
    /// user-facing dispatcher hint, this never substitutes the conventional
    /// 1280x800 default: an app observation must become uncertain when the
    /// current geometry cannot be re-observed.
    pub(crate) async fn exact_live_viewport(&self) -> Option<(u32, u32)> {
        self.query_live_viewport().await
    }

    /// Query the active session's viewport via `agent-browser eval`. The
    /// CLI returns the literal value of the JavaScript expression as
    /// stdout; the {w, h} object becomes a JSON-shaped string we parse
    /// back. Returns `None` on any failure path (CLI error, parse
    /// failure, missing session); the caller falls back to the
    /// agent-browser default. Cheap — eval round-trips are sub-10ms in
    /// the hot session.
    async fn query_live_viewport(&self) -> Option<(u32, u32)> {
        // 2026-05-24 — Must use `run_command` (NOT `run_cli_command`) so the
        // `--session <id>` flag is passed. `eval` is a session-scoped command
        // that needs to target the current browser; without the session flag
        // agent-browser falls back to the `default` session and spawns its own
        // daemon + Chromium (visible as an extra blank window). See diagnosis
        // in this commit's message.
        let result = self
            .session
            .run_command(&[
                "eval",
                "JSON.stringify({w: window.innerWidth, h: window.innerHeight})",
            ])
            .await
            .ok()?;
        if !result.success {
            return None;
        }
        let raw = result.stdout.trim();
        let unquoted = raw.trim_matches('"').replace("\\\"", "\"");
        let parsed: serde_json::Value = serde_json::from_str(&unquoted).ok()?;
        let w = parsed.get("w")?.as_u64()?;
        let h = parsed.get("h")?.as_u64()?;
        if w == 0 || h == 0 {
            return None;
        }
        Some((w as u32, h as u32))
    }

    async fn artifact_root_snapshot(&self) -> Result<Option<BTreeSet<PathBuf>>> {
        let Some(root) = self.artifact_root.as_deref() else {
            return Ok(None);
        };
        tokio::fs::create_dir_all(root).await?;
        collect_artifact_root_files(root).map(Some)
    }

    async fn artifacts_created_since(
        &self,
        before: Option<BTreeSet<PathBuf>>,
    ) -> Result<Vec<PrimitiveArtifact>> {
        let (Some(root), Some(before)) = (self.artifact_root.as_deref(), before) else {
            return Ok(Vec::new());
        };
        artifacts_created_in_root_since(root, &before)
    }

    /// The reason this invocation would submit a form, if it would.
    ///
    /// Probe failures (no box, a DOM-only engine, a CLI error) answer "not a
    /// submission" and are logged: the guard reads the page, and a page it
    /// cannot read is recorded rather than turned into a refusal of ordinary
    /// browsing.
    async fn submission_refusal(&self, invocation: &BrowserCliInvocation) -> Option<String> {
        use super::submit_guard::{
            click_submission_reason, enter_submission_reason, label_is_final_action,
            script_submission_reason, submit_probes, SubmitProbe, FOCUSED_FACTS_SCRIPT,
        };
        for probe in submit_probes(&invocation.artifact_commands, invocation.stdin.as_deref()) {
            let reason = match &probe {
                SubmitProbe::Click(target) => self
                    .control_facts_for_target(target)
                    .await
                    .and_then(|facts| click_submission_reason(&facts)),
                SubmitProbe::ClickAt(x, y) => self
                    .control_facts_at(*x, *y)
                    .await
                    .and_then(|facts| click_submission_reason(&facts)),
                SubmitProbe::EnterIn(target) => self
                    .control_facts_for_target(target)
                    .await
                    .and_then(|facts| enter_submission_reason(&facts)),
                SubmitProbe::EnterInFocused => self
                    .eval_control_facts(FOCUSED_FACTS_SCRIPT)
                    .await
                    .and_then(|facts| enter_submission_reason(&facts)),
                SubmitProbe::ClickByLabel(words) => words
                    .iter()
                    .find(|word| label_is_final_action(word))
                    .map(|word| format!("a \"{}\" control", word.trim())),
                SubmitProbe::Script(script) => script_submission_reason(script),
            };
            if reason.is_some() {
                return reason;
            }
        }
        None
    }

    /// The page as the owner would see it: the draft and title with delivered
    /// secrets scrubbed, and the draft left out while a capture is withheld.
    async fn page_facts_for_owner(&self) -> (super::submit_guard::PageFacts, [u8; 32]) {
        use super::submit_guard::{PageFacts, PAGE_FACTS_SCRIPT};
        let mut page = self
            .session
            .run_command(&["eval", "--json", PAGE_FACTS_SCRIPT])
            .await
            .ok()
            .and_then(|result| result.parsed_json)
            .and_then(|json| json.get("data")?.get("result").cloned())
            .and_then(|value| serde_json::from_value::<PageFacts>(value).ok())
            .unwrap_or_default();
        let fingerprint = page.submission_fingerprint();
        let withheld = self
            .browser_capture_withheld
            .as_ref()
            .map(|withheld| withheld.load(std::sync::atomic::Ordering::Acquire))
            .unwrap_or(CAPTURE_WITHHELD_NONE);
        page.title = self.scrub_with_delivered(&page.title);
        page.text = if withheld > CAPTURE_WITHHELD_NONE {
            String::new()
        } else {
            self.scrub_with_delivered(&page.text)
        };
        (page, fingerprint)
    }

    /// Put a held submission to the owner: a standing grant for the page's
    /// site passes it, otherwise a decision card asks. `Unavailable` when
    /// there is nobody to ask (no approval channel in this run).
    ///
    /// The page is read before the invocation runs, so an invocation that
    /// itself navigates, or switches tab or frame, may submit somewhere else.
    /// For those no standing grant applies and none is created: the owner is
    /// asked every time, about a destination the card says it cannot confirm.
    async fn confirm_submission(
        &self,
        reason: &str,
        invocation: &BrowserCliInvocation,
    ) -> SubmissionAnswer {
        use super::submission_grants::{self, SubmissionGrant};
        use super::submit_guard::{
            approval_request, changes_destination, APPROVE_ALWAYS, APPROVE_ONCE,
        };
        let Some(approver) = self.submission_approver.as_ref() else {
            return SubmissionAnswer::Unavailable;
        };
        let moves = changes_destination(&invocation.artifact_commands);
        let (page, fingerprint) = self.page_facts_for_owner().await;
        let origin = if moves {
            None
        } else {
            submission_grants::origin_of(&page.url)
        };
        if let Some(origin) = origin.as_deref() {
            if submission_grants::is_granted(&approver.grants_path, origin) {
                return SubmissionAnswer::Approved("standing grant");
            }
        }
        let response = approver
            .service
            .ask(approval_request(
                reason,
                &page,
                &approver.asker,
                origin.is_some(),
                moves,
            ))
            .await;
        let approved_always = response.decision == APPROVE_ALWAYS && origin.is_some();
        if response.decision != APPROVE_ONCE && !approved_always {
            return SubmissionAnswer::Declined;
        }
        // The answer can take a long time and the page can move meanwhile: the
        // approval was for the page the owner was shown, not whatever is there
        // now.
        let (now, current_fingerprint) = self.page_facts_for_owner().await;
        if !moves
            && !super::submit_guard::approval_matches_page(
                &page,
                fingerprint,
                &now,
                current_fingerprint,
                approved_always,
            )
        {
            tracing::warn!(
                "[OUTWARD-DECLINED] the submission page or draft changed while approval was pending"
            );
            return SubmissionAnswer::Declined;
        }
        if let (true, Some(origin)) = (approved_always, origin) {
            let grant = SubmissionGrant {
                origin,
                granted_at_ms: chrono::Utc::now().timestamp_millis(),
                request_id: response.request_id.clone(),
                agent_id: approver.asker.agent_id.clone(),
            };
            if let Err(error) = submission_grants::grant(&approver.grants_path, grant) {
                tracing::warn!(%error, "could not store the submission grant; approving once");
            }
            return SubmissionAnswer::Approved("always");
        }
        SubmissionAnswer::Approved("once")
    }

    async fn control_facts_for_target(
        &self,
        target: &str,
    ) -> Option<super::submit_guard::ControlFacts> {
        if let Some(facts) = self.control_facts_by_box(target).await {
            return Some(facts);
        }
        // Below the fold `elementFromPoint` sees nothing; the click itself
        // would scroll the element into view, so doing it first changes
        // nothing the command was not about to change.
        let _ = self.session.run_command(&["scrollintoview", target]).await;
        self.control_facts_by_box(target).await
    }

    async fn control_facts_by_box(
        &self,
        target: &str,
    ) -> Option<super::submit_guard::ControlFacts> {
        let result = self
            .session
            .run_command(&["get", "box", target, "--json"])
            .await
            .ok()?;
        let data = result.parsed_json.as_ref()?.get("data")?;
        let x = data.get("x")?.as_f64()?;
        let y = data.get("y")?.as_f64()?;
        let width = data.get("width")?.as_f64()?;
        let height = data.get("height")?.as_f64()?;
        self.control_facts_at(x + width / 2.0, y + height / 2.0)
            .await
    }

    async fn control_facts_at(&self, x: f64, y: f64) -> Option<super::submit_guard::ControlFacts> {
        self.eval_control_facts(&super::submit_guard::facts_at_point_script(x, y))
            .await
    }

    async fn eval_control_facts(&self, script: &str) -> Option<super::submit_guard::ControlFacts> {
        let outcome = self.session.run_command(&["eval", "--json", script]).await;
        let facts = outcome
            .as_ref()
            .ok()
            .and_then(|result| result.parsed_json.as_ref())
            .and_then(|json| json.get("data"))
            .and_then(|data| data.get("result"))
            .filter(|value| !value.is_null())
            .and_then(|value| serde_json::from_value(value.clone()).ok());
        if facts.is_none() {
            tracing::debug!("[BROWSER-SUBMIT-GUARD] could not inspect the target control");
        }
        facts
    }

    async fn run_command(
        &self,
        invocation: &BrowserCliInvocation,
    ) -> Result<AgentBrowserToolResult> {
        let arg_refs: Vec<&str> = invocation.argv.iter().map(String::as_str).collect();
        let stdin = invocation.stdin.as_deref();
        if let Some(root) = self.artifact_root.as_deref() {
            tokio::fs::create_dir_all(root).await?;
            self.session
                .run_command_with_options_and_stdin(
                    &arg_refs,
                    self.command_timeout_secs,
                    &[(AGENT_BROWSER_SCREENSHOT_DIR_ENV, root)],
                    stdin,
                )
                .await
        } else {
            self.session
                .run_command_with_options_and_stdin(
                    &arg_refs,
                    self.command_timeout_secs,
                    &[],
                    stdin,
                )
                .await
        }
    }

    /// Lower secret references for one CLI invocation (P4, plan §5.2).
    ///
    /// Only the CLI process's own stdin is a sink (a `--password-stdin`
    /// style option): the value reaches that process and nothing else. The
    /// command line is not — argv is visible to every process on the host —
    /// and neither is a page command inside a `batch` (`fill @e3 <ref>`),
    /// which would type the value into whichever page is active with no
    /// destination check; both are refused with the typed operation named.
    /// A one-time code never rides the CLI at all: it is destination-bound
    /// and delivered only by `browser__secure_prompt_fill`.
    fn resolve_invocation_secrets(
        &self,
        mut invocation: BrowserCliInvocation,
    ) -> Result<(BrowserCliInvocation, KnownSecretValues)> {
        if !invocation_contains_secret_placeholder(&invocation) {
            return Ok((invocation, KnownSecretValues::new()));
        }
        if invocation
            .artifact_commands
            .iter()
            .flatten()
            .any(|token| contains_secret_placeholder(token))
            || invocation
                .argv
                .iter()
                .any(|arg| contains_secret_placeholder(arg))
        {
            bail!(
                "a browser command line or page command cannot carry a secret reference (it would \
                 be visible on the host and typed into an unverified page); deliver a password with \
                 `browser__secure_prompt_fill` (`fields[].value` set to the reference) or, for a CLI \
                 primitive that reads it from stdin, pass it in `stdin`"
            );
        }
        let Some(store) = self.secret_store.as_deref() else {
            bail!(
                "browser command contains a secret placeholder, but no secret store is available; request the missing value with need_user_input first"
            );
        };
        let scope = self.ephemeral_secret_scope_id.as_deref();
        let mut known_values = KnownSecretValues::new();

        if let Some(stdin) = invocation.stdin.as_mut() {
            // Only the CLI's own credential intake reads stdin as a secret:
            // `auth … --password-stdin`. Any other primitive's stdin is data
            // the CLI may hand to the page (a script, a value), which is no
            // destination-checked sink.
            if !invocation_reads_a_password_from_stdin(&invocation.argv) {
                bail!(
                    "a browser command's stdin is not a credential sink unless it is `auth … \
                     --password-stdin`; deliver the value with `browser__secure_prompt_fill` \
                     (`fields[].value` set to the reference)"
                );
            }
            if let Some(scope) = scope {
                for (_, key) in crate::magician_v2::secrets::sinks::placeholders(stdin) {
                    if store.one_time_state(scope, &key).is_some() {
                        bail!(
                            "a one-time code is delivered only by `browser__secure_prompt_fill` (set \
                             `fields[].value` to the reference), never through the CLI"
                        );
                    }
                    if let Some(bound) =
                        crate::magician_v2::secrets::sinks::bound_destination(store, scope, &key)
                    {
                        bail!(
                            "the material for '{key}' is bound to {bound}; the CLI's credential intake is \
                             not that destination — deliver it with `browser__secure_prompt_fill`"
                        );
                    }
                }
            }
            let (resolved, values) = resolve_inline_placeholders(stdin, store, scope);
            known_values.extend(values);
            *stdin = resolved;
        }

        if invocation_contains_secret_placeholder(&invocation) {
            bail!(
                "browser command contains unresolved secret placeholders; use need_user_input for the missing value before dispatch"
            );
        }

        if let Some(delivered) = self.delivered_secret_values.as_ref() {
            if let Ok(mut delivered) = delivered.lock() {
                for (key, value) in known_values.iter() {
                    let delivery = format!("{key}@{}", delivered.len());
                    delivered.insert(delivery, value.clone());
                }
            }
        }
        Ok((invocation, known_values))
    }
}

/// The one CLI primitive that reads a secret from its own stdin.
fn invocation_reads_a_password_from_stdin(argv: &[String]) -> bool {
    argv.first().map(String::as_str) == Some("auth")
        && argv.iter().any(|arg| arg == "--password-stdin")
}

/// Commands that produce pixels the value could be visible in.
fn invocation_captures_pixels(invocation: &BrowserCliInvocation) -> bool {
    invocation.artifact_commands.iter().any(|command| {
        matches!(
            command.first().map(String::as_str),
            Some("screenshot" | "pdf" | "record")
        )
    })
}

/// Commands after which the page no longer shows what was typed: a
/// navigation or a reload. A click or key press is not one — it may be the
/// submission, or it may be a "show password" toggle.
fn invocation_moves_the_page(invocation: &BrowserCliInvocation) -> bool {
    invocation.artifact_commands.iter().any(|command| {
        matches!(
            command.first().map(String::as_str),
            Some("open" | "goto" | "navigate" | "back" | "forward" | "reload" | "close")
        )
    })
}

/// Commands whose output is the whole page's own text, which can prove a
/// typed value is no longer on it. A partial read (`get url`, `get text
/// <selector>`) proves nothing about the rest of the page and is not one.
fn invocation_observes_page_text(invocation: &BrowserCliInvocation) -> bool {
    invocation.artifact_commands.iter().any(|command| {
        matches!(
            command.first().map(String::as_str),
            Some("snapshot" | "text" | "html")
        )
    })
}

fn attach_host_page_evidence(result: &mut AgentBrowserToolResult, evidence: Option<Value>) {
    if !result.parsed_json.as_ref().is_some_and(Value::is_object) {
        let data = result
            .parsed_json
            .take()
            .unwrap_or_else(|| Value::String(result.stdout.clone()));
        result.parsed_json = Some(serde_json::json!({"data": data}));
    }
    let projected = result
        .parsed_json
        .as_mut()
        .and_then(Value::as_object_mut)
        .expect("object established above");
    projected.insert(
        "__magician_evidence_authority".into(),
        Value::String("host".into()),
    );
    projected.insert(
        "evidence".into(),
        Value::Array(evidence.into_iter().collect()),
    );
    result.stdout = result
        .parsed_json
        .as_ref()
        .expect("object established above")
        .to_string();
}

fn invocation_reads_page(invocation: &BrowserCliInvocation) -> bool {
    invocation.artifact_commands.iter().any(|command| {
        matches!(
            command.first().map(String::as_str),
            Some("snapshot" | "text" | "html" | "eval")
        ) || (command.first().map(String::as_str) == Some("get")
            && matches!(command.get(1).map(String::as_str), Some("text" | "html")))
    })
}

fn browser_page_evidence(capture: &AgentBrowserToolResult) -> Option<Value> {
    if !capture.success || capture.stdout_truncated || capture.stdout.len() > 64_000 {
        return None;
    }
    let mut value = capture
        .parsed_json
        .clone()
        .or_else(|| serde_json::from_str::<Value>(capture.stdout.trim()).ok())?;
    for _ in 0..4 {
        if let Value::String(text) = &value {
            value = serde_json::from_str(text).ok()?;
        } else if value.get("url").is_some() && value.get("text").is_some() {
            break;
        } else {
            value = value
                .pointer("/data/result")
                .or_else(|| value.get("data"))
                .or_else(|| value.get("result"))?
                .clone();
        }
    }
    let url = value.get("url")?.as_str()?;
    let canonical_url = crate::magician_v2::content_sources::canonicalize_http_url(url).ok()?;
    let text = value.get("text")?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    Some(serde_json::json!({
        "canonical_url": canonical_url,
        "title": value.get("title").and_then(Value::as_str),
        "text": text.chars().take(12_000).collect::<String>(),
        "fetch_status": if value.get("truncated").and_then(Value::as_bool) == Some(true) { "partial" } else { "complete" },
        "evidence_role": "opened_page", "claim_eligible": true,
        "retrieved_by": "browser_host_page_capture", "final_observation_only": true,
    }))
}

/// Commands that act on the page without returning any of its content — the
/// only ones that run while a value split across fields may still be on it.
/// Anything not listed (a snapshot, `get`, `eval`, `find`, a capture) is
/// withheld at that level: the list is what is known not to read, not what
/// is known to.
fn invocation_only_acts(invocation: &BrowserCliInvocation) -> bool {
    !invocation.artifact_commands.is_empty()
        && invocation.artifact_commands.iter().all(|command| {
            matches!(
                command.first().map(String::as_str),
                Some(
                    "open"
                        | "goto"
                        | "navigate"
                        | "back"
                        | "forward"
                        | "reload"
                        | "close"
                        | "click"
                        | "dblclick"
                        | "press"
                        | "type"
                        | "fill"
                        | "scroll"
                        | "hover"
                        | "focus"
                        | "select"
                        | "check"
                        | "uncheck"
                        | "wait"
                        | "mouse"
                        | "key"
                        | "keyboard"
                )
            )
        })
}

impl BrowserDispatcher {
    fn delivered_replacements(&self) -> Vec<String> {
        self.delivered_secret_values
            .as_ref()
            .and_then(|delivered| delivered.lock().ok())
            .map(|delivered| known_value_replacements(&delivered))
            .unwrap_or_default()
    }

    fn text_mentions_delivered_value(&self, text: &str) -> bool {
        self.delivered_replacements()
            .iter()
            .any(|value| text.contains(value.as_str()))
    }

    fn scrub_with_delivered(&self, text: &str) -> String {
        let replacements = self.delivered_replacements();
        replacements.iter().fold(text.to_string(), |acc, value| {
            acc.replace(value, "[REDACTED]")
        })
    }

    /// The run's delivered set covers what this invocation did not lower
    /// itself: a code the fill typed earlier, a password another action sent.
    fn scrub_result_with_delivered(&self, result: &mut AgentBrowserToolResult) {
        let replacements = self.delivered_replacements();
        if replacements.is_empty() {
            return;
        }
        let mut union = KnownSecretValues::new();
        for (index, value) in replacements.into_iter().enumerate() {
            union.insert(format!("delivered@{index}"), value);
        }
        sanitize_agent_browser_result(result, &union);
    }
}

/// What an image capture returns while a typed secret may still be visible.
fn withheld_capture_result() -> PrimitiveToolResult {
    let note = "capture withheld: a secret was just entered on this page and pixels cannot be \
                redacted. Navigate, or take a text snapshot of the settled page, then capture again.";
    withheld_result(note)
}

/// What any page-reading command returns while a value split across fields
/// may still be on the page.
fn submission_refused_result(reason: &str) -> PrimitiveToolResult {
    let note = super::submit_guard::refusal_note(reason);
    PrimitiveToolResult {
        success: false,
        stdout: String::new(),
        stderr: note.clone(),
        parsed_json: Some(serde_json::json!({
            "status": "submission_refused",
            "reason": reason,
            "note": note,
        })),
        artifacts: Vec::new(),
        elapsed_ms: 0,
    }
}

fn submission_declined_result(reason: &str) -> PrimitiveToolResult {
    let note = super::submit_guard::declined_note(reason);
    PrimitiveToolResult {
        success: false,
        stdout: String::new(),
        stderr: note.clone(),
        parsed_json: Some(serde_json::json!({
            "status": "submission_declined",
            "reason": reason,
            "note": note,
        })),
        artifacts: Vec::new(),
        elapsed_ms: 0,
    }
}

fn withheld_page_result() -> PrimitiveToolResult {
    let note = "observation withheld: a code was just entered digit by digit on this page and \
                cannot be redacted from text or pixels. Submit if you have not, then navigate \
                (`reload`, or `open` the page you expect) before observing again.";
    withheld_result(note)
}

fn withheld_result(note: &str) -> PrimitiveToolResult {
    PrimitiveToolResult {
        success: false,
        stdout: String::new(),
        stderr: note.to_string(),
        parsed_json: Some(serde_json::json!({ "status": "capture_withheld", "reason": note })),
        artifacts: Vec::new(),
        elapsed_ms: 0,
    }
}

/// Translate the read-only `help` primitive into exact agent-browser CLI argv.
/// This intentionally does not lazy-connect to Chrome.
pub fn help_tool_call_to_cli_args(args: &Value) -> Result<Vec<String>> {
    let obj = args.as_object().cloned().unwrap_or_default();
    let argv = optional_string_array(&obj, "args")?.unwrap_or_default();
    if !argv.is_empty() {
        // `help` runs its argv as given, so it may only ask for help: a
        // `skills` lookup or `<command> [sub] --help`, plain words only.
        // Anything else would run a real command past every check.
        let plain = |token: &String| {
            !token.is_empty()
                && token
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        };
        let asks_help = argv
            .last()
            .is_some_and(|last| last == "--help" || last == "-h");
        let skills = argv[0] == "skills";
        if !argv.iter().all(plain)
            || !(asks_help || skills)
            || (skills && argv.iter().any(|t| t.starts_with('-') && t != "--full"))
        {
            bail!(
                "`help` takes `[\"<command>\", \"--help\"]` or `[\"skills\", \"get\", \"<name>\"]`; run commands with their own tool"
            );
        }
        local_access_check(std::slice::from_ref(&argv))?;
        return validate_argv(argv);
    }

    if let Some(command) = obj
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        validate_command_token(command)?;
        if command == "core" {
            return Ok(vec![
                "skills".to_string(),
                "get".to_string(),
                "core".to_string(),
                "--full".to_string(),
            ]);
        }
        return Ok(vec![command.to_string(), "--help".to_string()]);
    }

    Ok(vec!["--help".to_string()])
}

/// One executable `agent-browser` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserCliInvocation {
    pub argv: Vec<String>,
    pub stdin: Option<String>,
    pub(super) artifact_commands: Vec<Vec<String>>,
}

/// Translate one inner-loop call into exact `agent-browser` argv.
///
/// The normal form is `<command>({ args })`, where `args` are precisely the
/// tokens after the command. `batch({ commands })` is the only structured
/// extension: `commands` is serialized to JSON stdin for the CLI's native
/// batch stdin mode, avoiding command-string parsing.
pub fn tool_call_to_cli_invocation(tool_name: &str, args: &Value) -> Result<BrowserCliInvocation> {
    let obj = args
        .as_object()
        .ok_or_else(|| anyhow!("`{tool_name}` arguments must be an object"))?;

    let command_args = optional_string_array(obj, "args")?.unwrap_or_default();
    if tool_name == "batch" {
        let invocation = batch_invocation(command_args, optional_command_matrix(obj, "commands")?)?;
        local_access_check(&invocation.artifact_commands)?;
        return Ok(invocation);
    }
    if obj.contains_key("commands") {
        bail!("`commands` is only supported by `batch`");
    }

    let stdin = optional_string(obj, "stdin")?;
    let command_args = navigation_args(tool_name, obj, command_args)?;
    let argv = cli_argv(tool_name, command_args)?;
    local_access_check(std::slice::from_ref(&argv))?;
    Ok(BrowserCliInvocation {
        artifact_commands: vec![argv.clone()],
        argv,
        stdin,
    })
}

// ---------------------------------------------------------------------------
// Local files and local services: the agent-browser daemon and its Chrome run
// outside every OS box, so what they may open is decided here, per command.
// ---------------------------------------------------------------------------

/// Schemes a navigation may use. Anything that names a local file, a
/// browser-internal page or script is refused; `file:` only while no goal
/// workspace exists, and then only for a path the acting agent may read.
const ALLOWED_NAVIGATION_SCHEMES: &[&str] = &["http", "https", "about", "data", "blob"];
/// Schemes recognised as schemes (so `localhost:3000` stays a host).
const KNOWN_SCHEMES: &[&str] = &[
    "http",
    "https",
    "about",
    "data",
    "blob",
    "file",
    "view-source",
    "filesystem",
    "chrome",
    "chrome-extension",
    "chrome-untrusted",
    "devtools",
    "javascript",
    "ftp",
    "ws",
    "wss",
];

/// Why `url` may not be opened by the acting agent's browser, or `None`.
pub(crate) fn browser_url_refusal(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let lower = trimmed.to_ascii_lowercase();
    // A bare local path is opened as a file.
    if trimmed.starts_with('/') || trimmed.starts_with("~/") {
        return local_file_refusal(&expand_home(trimmed), false)
            .or_else(|| file_scheme_refusal(trimmed));
    }
    let scheme = lower
        .split_once(':')
        .map(|(scheme, _)| scheme)
        .filter(|scheme| KNOWN_SCHEMES.contains(scheme));
    match scheme {
        None => loopback_service_navigation_refusal(&format!("https://{trimmed}")),
        Some("file") => {
            if let Some(refusal) = file_scheme_refusal(trimmed) {
                return Some(refusal);
            }
            let path = url::Url::parse(trimmed)
                .ok()
                .and_then(|parsed| parsed.to_file_path().ok())?;
            local_file_refusal(&path, false)
        },
        Some(scheme) if ALLOWED_NAVIGATION_SCHEMES.contains(&scheme) => {
            if matches!(scheme, "http" | "https") {
                loopback_service_navigation_refusal(trimmed)
            } else if matches!(scheme, "data" | "blob")
                && crate::magician_v2::execution::goal_workspace_access::denies_for_current()
                    .guards_workspaces
            {
                // A page this agent wrote could redirect or script its way
                // anywhere; a fenced agent opens only web pages.
                Some(format!(
                    "The browser may not open `{scheme}:` pages for an agent fenced from a goal workspace"
                ))
            } else {
                None
            }
        },
        Some(scheme) => Some(format!(
            "The browser may not open `{scheme}:` pages; use an http(s) URL"
        )),
    }
}

/// Whether `text` holds a `file:` URL scheme anywhere: `file:` at the start
/// or after a character no scheme name contains (`'file:///…'`,
/// `url=file:…`, `view-source:file:…`). A word that merely ends in "file"
/// (`profile:`, `myfile:` in page script) is not a scheme.
fn names_file_scheme(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    lower.match_indices("file:").any(|(at, _)| {
        lower[..at].chars().next_back().is_none_or(|before| {
            !(before.is_ascii_alphanumeric() || matches!(before, '+' | '.' | '-'))
        })
    })
}

/// `file:` pages are refused outright while any goal workspace exists: a
/// directory listing would let later clicks reach a workspace without
/// another check.
fn file_scheme_refusal(url: &str) -> Option<String> {
    crate::magician_v2::execution::goal_workspace_access::denies_for_current()
        .guards_workspaces
        .then(|| {
            format!(
                "The browser may not open local files (`{url}`) while goal workspaces exist; read files with read_file"
            )
        })
}

/// A local service the browser may not open: the supervisor, Magicutor, a
/// CDP endpoint and the notes server for every agent; Magician's API and
/// the UI dev servers too for an agent fenced from a goal workspace.
fn loopback_service_navigation_refusal(url: &str) -> Option<String> {
    use crate::magician_v2::execution::network_policy;
    use crate::magician_v2::execution::shell_box::{loopback_services, LoopbackService};
    let fenced = crate::magician_v2::execution::goal_workspace_access::denies_for_current()
        .guards_workspaces;
    // For a fenced agent, a target that is no URL with a host is refused
    // rather than left to the browser to interpret.
    let unparsed = || {
        fenced.then(|| {
            format!(
                "The browser may not open `{url}`: it is not a web address this agent may check"
            )
        })
    };
    let Some(parsed) = url::Url::parse(url).ok() else {
        return unparsed();
    };
    let Some(host) = parsed.host_str() else {
        return unparsed();
    };
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_string();
    let Some(port) = parsed.port_or_known_default() else {
        return unparsed();
    };
    // Resolved, not read by name: `localtest.me`, `localhost.`, an
    // IPv4-mapped address and the host's own LAN address all count.
    let addrs: Vec<std::net::IpAddr> = match host.parse::<std::net::IpAddr>() {
        Ok(ip) => vec![ip],
        Err(_) => {
            use std::net::ToSocketAddrs;
            (host.as_str(), port)
                .to_socket_addrs()
                .map(|addrs| addrs.map(|addr| addr.ip()).collect())
                .unwrap_or_default()
        },
    };
    let local = host.eq_ignore_ascii_case("localhost")
        || addrs.iter().any(|ip| network_policy::is_local_address(*ip));
    // A harness session's grant channel, for every agent: a page there with
    // a stolen grant in `--headers` would act as that session.
    if local && crate::magician_v2::execution::plane::grant_channel::is_channel_port(port) {
        return Some(format!(
            "The browser may not open {url}: it is a harness grant channel agents may not reach"
        ));
    }
    if fenced
        && !network_policy::allowed_private(&host, port)
        && (local
            || addrs.is_empty()
            || addrs
                .iter()
                .any(|ip| !network_policy::is_public_unmapped(*ip)))
    {
        return Some(format!(
            "The browser may not open {url}: an agent fenced from a goal workspace opens only public pages"
        ));
    }
    if !local {
        return None;
    }
    let kind = loopback_services()
        .into_iter()
        .find(|(candidate, _)| *candidate == port)
        .map(|(_, kind)| kind)?;
    let always = !matches!(
        kind,
        LoopbackService::MagicianApi | LoopbackService::DevServer
    );
    (always || fenced).then(|| {
        format!(
            "The browser may not open {url}: it is a local service agents may not reach ({kind:?})"
        )
    })
}

/// The Chrome flags that send every request of a fenced agent's browser
/// (navigation, redirects, scripts, frames, loopback included) through the
/// egress proxy's strict listener, which refuses local, private and service
/// addresses after resolving them itself. WebRTC may not send UDP outside
/// the proxy. `None` when that listener is not running: the fenced agent
/// then gets no browser.
pub(crate) fn fenced_browser_args() -> Option<String> {
    fenced_browser_args_for(crate::magician_v2::execution::egress_proxy::strict_port()?).into()
}

/// An empty agent-browser config for fenced sessions, in the plane's
/// private folder (no OS box reaches it), so no `agent-browser.json` an
/// agent wrote can set the executable, proxy or profile.
pub(crate) fn fenced_browser_config() -> Option<PathBuf> {
    let path = crate::magician_v2::execution::plane::grant_channel::private_root()
        .join("agent-browser-fenced.json");
    if std::fs::read_to_string(&path).is_ok_and(|text| text.trim() == "{}") {
        return Some(path);
    }
    std::fs::write(&path, "{}\n").ok().map(|()| path)
}

fn fenced_browser_args_for(port: u16) -> String {
    format!(
        "--proxy-server=http://127.0.0.1:{port},--proxy-bypass-list=<-loopback>,\
         --force-webrtc-ip-handling-policy=disable_non_proxied_udp"
    )
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(path)),
        None => PathBuf::from(path),
    }
}

/// The goal workspace and notes-index refusal for the browser reading
/// (`write: false`) or writing `path`.
fn local_file_refusal(path: &Path, write: bool) -> Option<String> {
    use crate::magician_v2::execution::goal_workspace_access as access;
    access::hybrid_index_refusal(path).or_else(|| {
        access::refusal_for_current(
            path,
            if write {
                access::Op::Write
            } else {
                access::Op::Read
            },
        )
    })
}

/// A token that names a local file: an absolute or `~/` path, or a
/// `file:` URL.
fn local_path_token(token: &str) -> Option<PathBuf> {
    let token = token.trim();
    if token.starts_with('/') || token.starts_with("~/") {
        return Some(expand_home(token));
    }
    if token.to_ascii_lowercase().starts_with("file:") {
        return url::Url::parse(token).ok()?.to_file_path().ok();
    }
    None
}

/// agent-browser options only Magician sets: how and where the browser
/// launches (executable, launch args, extensions, init scripts, engine,
/// provider, CDP, auto-connect), whose profile, session, namespace,
/// saved state or config it uses, its proxy and certificate trust, file
/// access, and where it writes by default. agent-browser takes a global
/// option anywhere in argv and the last one wins, so an agent token naming
/// one would override Magician's (an unboxed program, no proxy, the owner's
/// Chrome or profile, another agent's session). Refused in every agent token,
/// `--flag value` and `--flag=value` alike.
const MAGICIAN_ONLY_OPTIONS: &[&str] = &[
    "--profile",
    "--restore",
    "--restore-save",
    "--restore-check-url",
    "--restore-check-text",
    "--restore-check-fn",
    "--session-name",
    "--state",
    "--auto-connect",
    "--session",
    "--namespace",
    "--executable-path",
    "--extension",
    "--init-script",
    "--args",
    "--proxy",
    "--proxy-bypass",
    "--ignore-https-errors",
    "--ca-cert",
    "--no-ca-cert",
    "--allow-file-access",
    "-p",
    "--provider",
    "--device",
    "--screenshot-dir",
    "--cdp",
    "--download-path",
    "--action-policy",
    "--confirm-actions",
    "--confirm-interactive",
    "--engine",
    "--idle-timeout",
    "--config",
    "--webgpu",
    "--no-webmcp",
    "--model",
    "--debug",
];

/// Commands an agent a goal workspace is fenced from may not run: they
/// attach to another browser (`connect`), run or install other programs
/// (`plugin`, `install`, `upgrade`, `doctor`, `mcp`, `chat`), open servers
/// (`dashboard`, `stream`), list the owner's Chrome profiles (`profiles`),
/// or open DevTools (`inspect`).
const FENCED_REFUSED_COMMANDS: &[&str] = &[
    "connect",
    "plugin",
    "install",
    "upgrade",
    "doctor",
    "mcp",
    "chat",
    "dashboard",
    "stream",
    "profiles",
    "inspect",
];

/// Options of navigation commands that take a value, so the value is never
/// read as the page to open (`open --headers '{…}' file:///…`).
const VALUE_OPTIONS: &[&str] = &[
    "--headers",
    "--llms",
    "--filter",
    "--timeout",
    "--allowed-domains",
    "--max-output",
    "--tags",
    "-s",
    "--selector",
    "--fps",
    "--contact-sheet-threshold",
    "--frame",
    "--baseline",
];

/// The option a token names (`--flag` or `--flag=value`), if it is one.
fn option_name(token: &str) -> Option<&str> {
    let token = token.trim();
    (token.starts_with('-') && token != "-" && token != "--")
        .then(|| token.split_once('=').map_or(token, |(name, _)| name))
}

/// The positional tokens of `args`: every token that is neither an option
/// nor the value of a value-taking option.
fn positional_tokens(args: &[String]) -> Vec<&String> {
    let mut out = Vec::new();
    let mut skip = false;
    for token in args {
        if skip {
            skip = false;
            continue;
        }
        if let Some(name) = option_name(token) {
            skip = !token.contains('=') && VALUE_OPTIONS.contains(&name);
            continue;
        }
        out.push(token);
    }
    out
}

/// The pages a command opens, from its grammar: every positional of
/// `open`/`goto`/`navigate`/`read`/`vitals`/`a11y`/`pushstate`, of
/// `tab new`, `diff url`, and `record start` after its output path.
fn navigation_targets<'a>(name: &str, args: &'a [String]) -> Vec<&'a String> {
    let first = args.first().map(String::as_str);
    match name {
        "open" | "goto" | "navigate" | "read" | "vitals" | "a11y" | "pushstate" => {
            positional_tokens(args)
        },
        "tab" if first == Some("new") => positional_tokens(&args[1..]),
        "diff" if first == Some("url") => positional_tokens(&args[1..]),
        "record" if first == Some("start") => {
            positional_tokens(&args[1..]).into_iter().skip(1).collect()
        },
        _ => Vec::new(),
    }
}

/// Every command an invocation runs (batch subcommands included): no
/// Magician-only option in any token; for an agent a workspace is fenced
/// from, no command that leaves its own browser and no `read` fetch of a
/// URL (the daemon fetches it outside the browser's proxy); every page it
/// opens ([`navigation_targets`]); no `file:` token at all while a goal
/// workspace exists; the files it writes (screenshots, PDFs, downloads,
/// HARs, traces, recordings, saved state) and every token naming a local
/// file it may read (uploads, state loads).
fn local_access_check(commands: &[Vec<String>]) -> Result<()> {
    let fenced = crate::magician_v2::execution::goal_workspace_access::denies_for_current()
        .guards_workspaces;
    for command in commands {
        let Some(name) = command.first().map(String::as_str) else {
            continue;
        };
        // Also as agent-browser would read a token it unquotes: quotes and
        // backslashes dropped, split on whitespace.
        let unquoted: Vec<String> = command
            .iter()
            .flat_map(|token| {
                token
                    .chars()
                    .filter(|c| !matches!(*c, '"' | '\'' | '\\'))
                    .collect::<String>()
                    .split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect();
        if let Some(option) = command
            .iter()
            .chain(unquoted.iter())
            .filter_map(|token| option_name(token))
            .find(|option| MAGICIAN_ONLY_OPTIONS.contains(option))
        {
            bail!(
                "The browser option `{option}` is set by Magician, not by an agent; leave it out"
            );
        }
        let args = &command[1..];
        if fenced && FENCED_REFUSED_COMMANDS.contains(&name) {
            bail!(
                "The browser command `{name}` is not available to an agent fenced from a goal workspace"
            );
        }
        let targets = navigation_targets(name, args);
        if fenced && name == "read" && !targets.is_empty() {
            bail!(
                "`read <url>` fetches outside the browser for an agent fenced from a goal workspace; \
                 open the page and run `read` without a URL"
            );
        }
        for target in targets {
            if let Some(refusal) = browser_url_refusal(target) {
                bail!(refusal);
            }
        }
        for token in command.iter().chain(unquoted.iter()) {
            let value = token
                .split_once('=')
                .map_or(token.as_str(), |(_, value)| value);
            if [token.as_str(), value]
                .iter()
                .any(|text| names_file_scheme(text))
            {
                if let Some(refusal) = file_scheme_refusal(token) {
                    bail!(refusal);
                }
            }
        }
        let mut outputs = artifact_paths_from_command(command);
        if name == "state" && args.first().map(String::as_str) == Some("save") {
            outputs.extend(
                args[1..]
                    .iter()
                    .filter(|arg| !arg.starts_with('-'))
                    .map(PathBuf::from),
            );
        }
        for path in &outputs {
            if let Some(refusal) = local_file_refusal(path, true) {
                bail!(refusal);
            }
        }
        for token in args {
            let value = token
                .split_once('=')
                .map_or(token.as_str(), |(_, value)| value);
            for candidate in [token.as_str(), value] {
                if let Some(path) = local_path_token(candidate) {
                    if outputs.contains(&path) {
                        continue;
                    }
                    if let Some(refusal) = local_file_refusal(&path, false) {
                        bail!(refusal);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Legacy helper used by tests and callers that only need argv.
pub fn tool_call_to_cli_args(tool_name: &str, args: &Value) -> Result<Vec<String>> {
    Ok(tool_call_to_cli_invocation(tool_name, args)?.argv)
}

fn batch_invocation(
    args: Vec<String>,
    commands: Option<Vec<Vec<String>>>,
) -> Result<BrowserCliInvocation> {
    let argv = cli_argv("batch", args)?;
    let Some(commands) = commands else {
        // agent-browser parses a batch string shell-style (quotes, escapes)
        // and no Magician tokenizer matches it exactly: a quoted token could
        // become an option or a target past every check. Agents send the
        // `commands` matrix, checked token by token.
        if let Some(command) = argv[1..].iter().find(|arg| !arg.starts_with('-')) {
            bail!(
                "`batch` takes its commands as `commands` (an array of argv arrays, e.g. \
                 [[\"open\", \"https://example.com\"], [\"snapshot\", \"-i\"]]), not as a string (`{command}`)"
            );
        }
        let artifact_commands = artifact_command_candidates(&argv);
        return Ok(BrowserCliInvocation {
            argv,
            stdin: None,
            artifact_commands,
        });
    };

    validate_batch_option_args(&argv[1..])?;
    validate_command_matrix(&commands)?;
    // Artifact detection inspects the LLM-authored commands, not the
    // spaced version sent to the CLI — synthetic waits never produce
    // artifacts.
    let mut artifact_commands = vec![argv.clone()];
    artifact_commands.extend(commands.clone());
    let spaced = space_batch_commands(
        commands,
        batch_command_spacing_ms(),
        batch_mutation_settle_ms(),
    );
    let stdin = serde_json::to_string(&spaced)?;
    Ok(BrowserCliInvocation {
        argv,
        stdin: Some(stdin),
        artifact_commands,
    })
}

/// Resolve the configured spacing (ms) between consecutive batch
/// subcommands. Reads `MAGICIAN_AGENT_BROWSER_BATCH_SPACING_MS` if set,
/// otherwise falls back to [`DEFAULT_BATCH_COMMAND_SPACING_MS`]. Invalid
/// or negative values fall back to the default.
pub(super) fn batch_command_spacing_ms() -> u64 {
    std::env::var(BATCH_COMMAND_SPACING_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_BATCH_COMMAND_SPACING_MS)
}

/// Resolve the extra settle delay (ms) inserted after a high-level mutating
/// subcommand. Reads `MAGICIAN_AGENT_BROWSER_MUTATION_SETTLE_MS` if set,
/// otherwise falls back to [`DEFAULT_BATCH_MUTATION_SETTLE_MS`]. Invalid values
/// fall back to the default.
pub(super) fn batch_mutation_settle_ms() -> u64 {
    std::env::var(BATCH_MUTATION_SETTLE_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_BATCH_MUTATION_SETTLE_MS)
}

/// Whether a subcommand is a HIGH-LEVEL mutation that triggers a page reaction
/// (event handlers + re-render) the next subcommand should wait out. Raw `mouse`
/// (move/down/up) is intentionally excluded — those are micro-steps of a single
/// gesture and only need the base spacing between them, not a full settle.
fn command_needs_settle(cmd: &[String]) -> bool {
    matches!(
        cmd.first().map(String::as_str),
        Some(
            "click"
                | "drag"
                | "fill"
                | "type"
                | "press"
                | "check"
                | "uncheck"
                | "select"
                | "tap"
                | "upload"
        )
    )
}

/// Insert a synthetic `["wait", "<ms>"]` subcommand between consecutive
/// subcommands so the page can settle between steps. The injected wait is
/// `settle_ms` (clamped to be at least the base) when the PRECEDING command was
/// a high-level mutation — it just changed the page and the next step should see
/// the settled result — otherwise the base `spacing_ms`. Insertion is skipped
/// when either neighbor is itself a `wait`, so caller-authored explicit waits
/// aren't doubled up. `spacing_ms == 0` disables all synthetic waits.
pub(super) fn space_batch_commands(
    commands: Vec<Vec<String>>,
    spacing_ms: u64,
    settle_ms: u64,
) -> Vec<Vec<String>> {
    if spacing_ms == 0 || commands.len() < 2 {
        return commands;
    }
    let mut spaced: Vec<Vec<String>> = Vec::with_capacity(commands.len() * 2 - 1);
    let mut prev_needs_settle = false;
    for cmd in commands {
        let curr_is_wait = matches!(cmd.first().map(String::as_str), Some("wait"));
        let prev_is_wait = matches!(
            spaced.last().and_then(|c| c.first()).map(String::as_str),
            Some("wait")
        );
        if !spaced.is_empty() && !prev_is_wait && !curr_is_wait {
            let wait_ms = if prev_needs_settle {
                settle_ms.max(spacing_ms)
            } else {
                spacing_ms
            };
            spaced.push(vec!["wait".to_string(), wait_ms.to_string()]);
        }
        prev_needs_settle = command_needs_settle(&cmd);
        spaced.push(cmd);
    }
    spaced
}

/// The page `open` / `goto` should load: `args[0]`, else a `url` key. Models
/// name the parameter `url` often enough that dropping it silently produced a
/// bare `agent-browser open` — a blank page reported as success. A navigation
/// with no target is an error the model can act on, not a no-op.
pub fn open_target(obj: &serde_json::Map<String, Value>) -> Result<Option<String>> {
    if let Some(first) = optional_string_array(obj, "args")?
        .unwrap_or_default()
        .into_iter()
        .find(|token| !token.starts_with("--"))
    {
        return Ok(Some(first));
    }
    Ok(optional_string(obj, "url")?
        .map(|url| url.trim().to_string())
        .filter(|url| !url.is_empty()))
}

fn navigation_args(
    command: &str,
    obj: &serde_json::Map<String, Value>,
    mut args: Vec<String>,
) -> Result<Vec<String>> {
    if !matches!(command, "open" | "goto") {
        return Ok(args);
    }
    let has_target = args.iter().any(|token| !token.starts_with("--"));
    if !has_target {
        match open_target(obj)? {
            Some(url) => args.insert(0, url),
            None => bail!(
                "`{command}` needs the page URL as args[0] (for example [\"https://example.com\"]); \
                 no URL was given"
            ),
        }
    }
    Ok(args)
}

fn cli_argv(command: &str, args: Vec<String>) -> Result<Vec<String>> {
    validate_command_token(command)?;
    validate_args(&args)?;
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(command.to_string());
    argv.extend(args);
    Ok(argv)
}

fn optional_string_array(
    obj: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<Vec<String>>> {
    let Some(value) = obj.get(key) else {
        return Ok(None);
    };
    let array = value
        .as_array()
        .ok_or_else(|| anyhow!("`{key}` must be an array of strings"))?;
    Ok(Some(string_array_from_values(array, key)?))
}

fn optional_string(obj: &serde_json::Map<String, Value>, key: &str) -> Result<Option<String>> {
    let Some(value) = obj.get(key) else {
        return Ok(None);
    };
    let text = value
        .as_str()
        .ok_or_else(|| anyhow!("`{key}` must be a string"))?
        .to_string();
    if text.contains('\0') {
        bail!("`{key}` contains NUL");
    }
    Ok(Some(text))
}

fn optional_command_matrix(
    obj: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<Vec<Vec<String>>>> {
    let Some(value) = obj.get(key) else {
        return Ok(None);
    };
    let commands = value
        .as_array()
        .ok_or_else(|| anyhow!("`{key}` must be an array of string arrays"))?;
    commands
        .iter()
        .enumerate()
        .map(|(idx, command)| {
            let array = command
                .as_array()
                .ok_or_else(|| anyhow!("`{key}[{idx}]` must be an array of strings"))?;
            string_array_from_values(array, &format!("{key}[{idx}]"))
        })
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

fn string_array_from_values(values: &[Value], field: &str) -> Result<Vec<String>> {
    values
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            value
                .as_str()
                .map(ToString::to_string)
                .ok_or_else(|| anyhow!("`{field}[{idx}]` must be a string"))
        })
        .collect()
}

fn validate_argv(argv: Vec<String>) -> Result<Vec<String>> {
    if argv.is_empty() {
        return Ok(argv);
    }
    validate_command_token(&argv[0])?;
    validate_args(&argv[1..])?;
    Ok(argv)
}

fn validate_command_token(command: &str) -> Result<()> {
    if command.trim().is_empty() {
        bail!("agent-browser command must be non-empty");
    }
    if command.chars().any(|c| c.is_whitespace() || c == '\0') {
        bail!("agent-browser command must be one argv token");
    }
    Ok(())
}

fn validate_args(args: &[String]) -> Result<()> {
    if let Some((idx, _)) = args.iter().enumerate().find(|(_, arg)| arg.contains('\0')) {
        bail!("agent-browser argv token at index {} contains NUL", idx + 1);
    }
    Ok(())
}

fn invocation_contains_secret_placeholder(invocation: &BrowserCliInvocation) -> bool {
    invocation
        .argv
        .iter()
        .any(|arg| contains_secret_placeholder(arg))
        || invocation
            .stdin
            .as_deref()
            .map(contains_secret_placeholder)
            .unwrap_or(false)
}

fn contains_secret_placeholder(text: &str) -> bool {
    text.contains("[REDACTED:") || text.contains("[REF:")
}

fn sanitize_agent_browser_result(
    result: &mut AgentBrowserToolResult,
    known_values: &KnownSecretValues,
) {
    let replacements = known_value_replacements(known_values);
    if replacements.is_empty() {
        return;
    }

    result.stdout = replace_known_values(&result.stdout, &replacements);
    result.stderr = replace_known_values(&result.stderr, &replacements);
    if let Some(parsed_json) = result.parsed_json.take() {
        result.parsed_json = Some(sanitize_json_value(parsed_json, &replacements));
    }
}

fn sanitize_text_with_known_values(input: &str, known_values: &KnownSecretValues) -> String {
    replace_known_values(input, &known_value_replacements(known_values))
}

fn replace_known_values(input: &str, replacements: &[String]) -> String {
    replacements.iter().fold(input.to_string(), |acc, value| {
        acc.replace(value, "[REDACTED]")
    })
}

fn sanitize_json_value(value: Value, replacements: &[String]) -> Value {
    map_json_strings_owned(
        value,
        |text| replace_known_values(&text, replacements),
        |_, _| None,
    )
}

fn validate_batch_option_args(args: &[String]) -> Result<()> {
    if let Some(arg) = args.iter().find(|arg| !arg.starts_with('-')) {
        bail!(
            "`batch.commands` uses JSON stdin; `args` may contain only batch options such as --bail or --json, got `{arg}`"
        );
    }
    Ok(())
}

fn validate_command_matrix(commands: &[Vec<String>]) -> Result<()> {
    for (idx, command) in commands.iter().enumerate() {
        let Some(name) = command.first() else {
            bail!("`commands[{idx}]` must not be empty");
        };
        validate_command_token(name)?;
        // A nested batch would carry its commands as strings past every
        // per-command check (targets, outputs, fenced commands).
        if name == "batch" {
            bail!(
                "`batch.commands[{idx}]` is itself a `batch`; put its commands in this `commands` matrix instead"
            );
        }
        if name == "help" {
            bail!(
                "`batch.commands[{idx}]` uses `help`, but `help` is a runtime inner-loop tool, not an agent-browser batch subcommand. Call standalone help({{\"args\":[\"<command>\",\"--help\"]}}) outside batch."
            );
        }
        validate_args(&command[1..])?;
    }
    Ok(())
}

fn artifacts_for_commands(
    commands: &[Vec<String>],
    stdout: &str,
    parsed_json: Option<&Value>,
) -> Vec<PrimitiveArtifact> {
    let mut artifacts = Vec::new();

    for command in commands {
        for path in artifact_paths_from_command(command) {
            push_artifact(&mut artifacts, command_name(command), path);
        }
    }

    if commands.iter().any(|command| is_artifact_command(command)) {
        for path in artifact_paths_from_stdout(stdout) {
            push_artifact(&mut artifacts, command_name_for_path(&path), path);
        }
    }

    if let Some(parsed_json) = parsed_json {
        for path in
            crate::magician_v2::execution::primitive_dispatch::artifacts::discover_artifact_paths(
                parsed_json,
            )
        {
            push_artifact(&mut artifacts, command_name_for_path(&path), path);
        }
    }

    artifacts
}

fn artifact_command_candidates(argv: &[String]) -> Vec<Vec<String>> {
    let mut commands = vec![argv.to_vec()];
    if argv.first().map(String::as_str) == Some("batch") {
        commands.extend(batch_subcommands_from_args(&argv[1..]));
    }
    commands
}

fn batch_subcommands_from_args(args: &[String]) -> Vec<Vec<String>> {
    let mut after_double_dash = false;
    let mut commands = Vec::new();
    for arg in args {
        if !after_double_dash && arg == "--" {
            after_double_dash = true;
            continue;
        }
        if !after_double_dash && arg.starts_with('-') {
            continue;
        }
        if let Some(command) = parse_batch_subcommand(arg) {
            commands.push(command);
        }
    }
    commands
}

fn parse_batch_subcommand(command: &str) -> Option<Vec<String>> {
    let tokens: Vec<String> = command
        .split_whitespace()
        .map(|token| token.trim_matches('"').trim_matches('\'').to_string())
        .filter(|token| !token.is_empty())
        .collect();
    (!tokens.is_empty()).then_some(tokens)
}

fn push_artifact(artifacts: &mut Vec<PrimitiveArtifact>, source_tool: &str, path: PathBuf) {
    let already_present = artifacts.iter().any(|artifact| {
        artifact
            .effective_path()
            .map(|candidate| candidate == path.as_path())
            .unwrap_or(false)
    });
    if already_present {
        return;
    }
    artifacts.push(PrimitiveArtifact::from_path(
        browser_artifact_kind(source_tool, &path),
        source_tool.to_string(),
        String::new(),
        path,
    ));
}

fn dedupe_artifacts_by_path(artifacts: &mut Vec<PrimitiveArtifact>) {
    let mut seen = BTreeSet::new();
    artifacts.retain(|artifact| {
        let Some(path) = artifact.effective_path() else {
            return true;
        };
        seen.insert(path.to_path_buf())
    });
}

fn collect_artifact_root_files(root: &Path) -> Result<BTreeSet<PathBuf>> {
    let mut files = BTreeSet::new();
    collect_artifact_root_files_inner(root, 0, &mut files)?;
    Ok(files)
}

fn collect_artifact_root_files_inner(
    dir: &Path,
    depth: usize,
    files: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    if depth > 4 || !dir.exists() {
        return Ok(());
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_file() {
            if has_known_artifact_extension(&path) {
                files.insert(path);
            }
        } else if file_type.is_dir() {
            collect_artifact_root_files_inner(&path, depth + 1, files)?;
        }
    }

    Ok(())
}

fn artifacts_created_in_root_since(
    root: &Path,
    before: &BTreeSet<PathBuf>,
) -> Result<Vec<PrimitiveArtifact>> {
    let after = collect_artifact_root_files(root)?;
    let mut artifacts = Vec::new();
    for path in after.difference(before) {
        push_artifact(&mut artifacts, command_name_for_path(path), path.clone());
    }
    Ok(artifacts)
}

fn command_name(command: &[String]) -> &str {
    command.first().map(String::as_str).unwrap_or("browser")
}

fn command_name_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("png" | "jpg" | "jpeg" | "webp") => "screenshot",
        Some("pdf") => "pdf",
        Some("har") => "network",
        Some("webm") => "record",
        _ => "browser",
    }
}

fn browser_artifact_kind(tool_name: &str, path: &Path) -> String {
    match tool_name {
        "screenshot" => "screenshot".to_string(),
        "pdf" => "pdf".to_string(),
        "download" => "download".to_string(),
        "network" => "har".to_string(),
        "trace" => "trace".to_string(),
        "profiler" => "profile".to_string(),
        "record" => "video".to_string(),
        _ => path
            .extension()
            .and_then(|value| value.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .filter(|ext| !ext.is_empty())
            .unwrap_or_else(|| tool_name.to_string()),
    }
}

fn is_artifact_command(command: &[String]) -> bool {
    matches!(
        command.first().map(String::as_str),
        Some("screenshot" | "pdf" | "download" | "network" | "trace" | "profiler" | "record")
    )
}

fn artifact_paths_from_command(command: &[String]) -> Vec<PathBuf> {
    let Some(name) = command.first().map(String::as_str) else {
        return Vec::new();
    };
    match name {
        "screenshot" => screenshot_path_from_args(&command[1..])
            .into_iter()
            .collect(),
        "pdf" => positional_arg(&command[1..], 0).into_iter().collect(),
        "download" => positional_arg(&command[1..], 1).into_iter().collect(),
        "network" if command.get(1).map(String::as_str) == Some("har") => tail_from(command, 3)
            .and_then(|tail| positional_arg(tail, 0))
            .into_iter()
            .collect(),
        "trace" | "profiler"
            if command.get(1).map(String::as_str) == Some("start")
                || command.get(1).map(String::as_str) == Some("stop") =>
        {
            positional_arg(&command[2..], 0).into_iter().collect()
        },
        "record" if command.get(1).map(String::as_str) == Some("start") => {
            positional_arg(&command[2..], 0).into_iter().collect()
        },
        _ => Vec::new(),
    }
}

fn positional_arg(args: &[String], index: usize) -> Option<PathBuf> {
    args.iter()
        .filter(|arg| !arg.starts_with('-'))
        .nth(index)
        .filter(|arg| looks_like_artifact_path(arg))
        .map(PathBuf::from)
}

fn tail_from(command: &[String], start: usize) -> Option<&[String]> {
    if command.len() >= start {
        Some(&command[start..])
    } else {
        None
    }
}

fn screenshot_path_from_args(args: &[String]) -> Option<PathBuf> {
    args.iter()
        .rev()
        .find(|arg| !arg.starts_with('-') && looks_like_artifact_path(arg))
        .map(PathBuf::from)
}

fn artifact_paths_from_stdout(stdout: &str) -> Vec<PathBuf> {
    stdout
        .lines()
        .flat_map(|line| line.split_whitespace())
        .map(|token| {
            token.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ':' | ';' | '(' | ')'))
        })
        .filter(|token| looks_like_artifact_path(token))
        .map(PathBuf::from)
        .collect()
}

fn looks_like_artifact_path(value: &str) -> bool {
    if value.starts_with("http://") || value.starts_with("https://") {
        return false;
    }
    let path = Path::new(value);
    has_known_artifact_extension(path)
}

fn has_known_artifact_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref(),
        Some(
            "png"
                | "jpg"
                | "jpeg"
                | "webp"
                | "pdf"
                | "har"
                | "json"
                | "webm"
                | "zip"
                | "csv"
                | "txt"
        )
    )
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn host_page_capture_keeps_source_provenance_and_rejects_failed_or_non_web_reads() {
        let mut capture = AgentBrowserToolResult {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
            parsed_json: Some(json!({"data": {"result": serde_json::to_string(&json!({
                "url":"https://example.org/report", "title":"Community research",
                "text":"Subscribers: 878000", "truncated": true,
            })).unwrap()}})),
            elapsed_ms: 1,
            stdout_truncated: false,
            stderr_truncated: false,
        };
        let evidence = browser_page_evidence(&capture).unwrap();
        assert_eq!(evidence["canonical_url"], "https://example.org/report");
        assert_eq!(evidence["text"], "Subscribers: 878000");
        assert_eq!(evidence["fetch_status"], "partial");
        assert_eq!(evidence["claim_eligible"], true);
        assert_eq!(evidence["final_observation_only"], true);
        let mut returned = capture.clone();
        returned.parsed_json = Some(json!({"data":{"result":"MODEL_RETURN"}}));
        attach_host_page_evidence(&mut returned, Some(evidence.clone()));
        assert_eq!(
            serde_json::from_str::<Value>(&returned.stdout).unwrap(),
            returned.parsed_json.clone().unwrap()
        );
        assert_eq!(
            returned.parsed_json.as_ref().unwrap()["evidence"][0],
            evidence
        );
        assert_eq!(
            returned.parsed_json.as_ref().unwrap()["__magician_evidence_authority"],
            "host"
        );
        returned.parsed_json = Some(json!(["MODEL_RETURN"]));
        attach_host_page_evidence(&mut returned, None);
        assert_eq!(
            returned.parsed_json.as_ref().unwrap()["evidence"],
            json!([])
        );
        assert!(returned
            .stdout
            .starts_with("{\"__magician_evidence_authority\""));
        capture.success = false;
        assert!(browser_page_evidence(&capture).is_none());
        capture.success = true;
        capture.stdout_truncated = true;
        assert!(browser_page_evidence(&capture).is_none());
        capture.stdout_truncated = false;
        capture.parsed_json = Some(json!({"url":"file:///private/config", "text":"secret"}));
        assert!(browser_page_evidence(&capture).is_none());
    }

    #[test]
    fn direct_tool_maps_exact_command_and_args() {
        let argv =
            tool_call_to_cli_args("click", &json!({"args": ["@e3", "--new-tab"]})).expect("argv");
        assert_eq!(argv, vec!["click", "@e3", "--new-tab"]);
    }

    #[test]
    fn direct_tool_accepts_optional_stdin_for_cli_primitives() {
        let invocation = tool_call_to_cli_invocation(
            "auth",
            &json!({
                "args": ["save", "work", "--password-stdin"],
                "stdin": "[REDACTED:password]"
            }),
        )
        .expect("invocation");

        assert_eq!(
            invocation.argv,
            vec!["auth", "save", "work", "--password-stdin"]
        );
        assert_eq!(invocation.stdin.as_deref(), Some("[REDACTED:password]"));
    }

    #[test]
    fn browser_dispatcher_resolves_secret_placeholders_at_final_boundary() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cli = temp.path().join("agent-browser");
        std::fs::write(&cli, b"#!/bin/sh\n").expect("fake cli");
        let session = Arc::new(
            AgentBrowserSession::new(
                "exec-1",
                crate::magician_v2::execution::primitive_dispatch::browser::session::ConnectionMode::Headless,
                cli,
            )
            .expect("session"),
        );
        let store = Arc::new(crate::magician_v2::secrets::SecretStore::new_empty(
            Box::new(crate::magician_v2::secrets::InMemoryKeyProvider::new()),
            temp.path().join("secrets"),
        ));
        store
            .register_ephemeral("execution:exec-1", "password", "s3cret".to_string())
            .expect("register secret");
        let dispatcher = BrowserDispatcher::with_options(session, None, 1)
            .with_secret_context(Some(store), Some("execution:exec-1".to_string()));
        let invocation = BrowserCliInvocation {
            argv: vec![
                "auth".to_string(),
                "save".to_string(),
                "work".to_string(),
                "--password-stdin".to_string(),
            ],
            stdin: Some("[REDACTED:password]".to_string()),
            artifact_commands: Vec::new(),
        };

        let (resolved, known_values) = dispatcher
            .resolve_invocation_secrets(invocation)
            .expect("resolved");
        assert_eq!(resolved.stdin.as_deref(), Some("s3cret"));
        assert_eq!(
            known_values.get("password").map(String::as_str),
            Some("s3cret")
        );

        let mut result = AgentBrowserToolResult {
            success: true,
            stdout: "saved s3cret".to_string(),
            stderr: String::new(),
            parsed_json: Some(json!({"value": "s3cret"})),
            elapsed_ms: 1,
            stdout_truncated: false,
            stderr_truncated: false,
        };
        sanitize_agent_browser_result(&mut result, &known_values);
        assert_eq!(result.stdout, "saved [REDACTED]");
        assert_eq!(result.parsed_json, Some(json!({"value": "[REDACTED]"})));
        assert_eq!(
            sanitize_text_with_known_values("spawn failed for s3cret", &known_values),
            "spawn failed for [REDACTED]"
        );
    }

    // P4 Task 4.4: the CLI command line and page commands are not sinks;
    // one-time material never rides the CLI at all.
    #[test]
    fn browser_command_lines_and_page_commands_refuse_secret_references() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cli = temp.path().join("agent-browser");
        std::fs::write(&cli, b"#!/bin/sh\n").expect("fake cli");
        let session = Arc::new(
            AgentBrowserSession::new(
                "exec-2",
                crate::magician_v2::execution::primitive_dispatch::browser::session::ConnectionMode::Headless,
                cli,
            )
            .expect("session"),
        );
        let store = Arc::new(crate::magician_v2::secrets::SecretStore::new_empty(
            Box::new(crate::magician_v2::secrets::InMemoryKeyProvider::new()),
            temp.path().join("secrets"),
        ));
        store
            .register_ephemeral("execution:exec-2", "password", "s3cret".to_string())
            .expect("register secret");
        store
            .register_one_time(
                "execution:exec-2",
                "otp",
                "042917".to_string(),
                chrono::Utc::now().timestamp_millis() + 60_000,
                crate::magician_v2::secrets::OneTimeBinding::default(),
            )
            .expect("register code");
        let delivered = Arc::new(std::sync::Mutex::new(KnownSecretValues::new()));
        let dispatcher = BrowserDispatcher::with_options(session, None, 1)
            .with_secret_context(Some(store), Some("execution:exec-2".to_string()))
            .with_delivery_tracking(Some(Arc::clone(&delivered)), None);

        // argv
        let err = dispatcher
            .resolve_invocation_secrets(BrowserCliInvocation {
                argv: vec!["fill".into(), "@e3".into(), "[REF:password]".into()],
                stdin: None,
                artifact_commands: vec![vec!["fill".into(), "@e3".into(), "[REF:password]".into()]],
            })
            .expect_err("argv is not a sink");
        assert!(
            err.to_string().contains("browser__secure_prompt_fill"),
            "{err}"
        );
        assert!(!err.to_string().contains("s3cret"));
        // a page command inside a batch, riding stdin
        let commands = vec![vec![
            "fill".to_string(),
            "@e3".to_string(),
            "[REF:password]".to_string(),
        ]];
        let err = dispatcher
            .resolve_invocation_secrets(BrowserCliInvocation {
                argv: vec!["batch".into(), "--bail".into()],
                stdin: Some(serde_json::to_string(&commands).unwrap()),
                artifact_commands: {
                    let mut all = vec![vec!["batch".to_string(), "--bail".to_string()]];
                    all.extend(commands.clone());
                    all
                },
            })
            .expect_err("a page command is not a sink");
        assert!(err.to_string().contains("page command"), "{err}");
        // a one-time code on the CLI's stdin
        let err = dispatcher
            .resolve_invocation_secrets(BrowserCliInvocation {
                argv: vec![
                    "auth".into(),
                    "save".into(),
                    "work".into(),
                    "--password-stdin".into(),
                ],
                stdin: Some("[REF:otp]".to_string()),
                artifact_commands: Vec::new(),
            })
            .expect_err("a code never rides the CLI");
        assert!(err.to_string().contains("one-time code"), "{err}");
        assert!(
            delivered.lock().unwrap().is_empty(),
            "a refusal delivers nothing"
        );
        // any other primitive's stdin is not a sink
        let err = dispatcher
            .resolve_invocation_secrets(BrowserCliInvocation {
                argv: vec!["eval".into(), "-".into()],
                stdin: Some("login('[REF:password]')".to_string()),
                artifact_commands: vec![vec!["eval".into(), "-".into()]],
            })
            .expect_err("only the auth intake reads a secret from stdin");
        assert!(err.to_string().contains("--password-stdin"), "{err}");
        // a password on the CLI's own credential intake still lowers, and the run remembers it
        let (resolved, _) = dispatcher
            .resolve_invocation_secrets(BrowserCliInvocation {
                argv: vec![
                    "auth".into(),
                    "save".into(),
                    "work".into(),
                    "--password-stdin".into(),
                ],
                stdin: Some("[REF:password]".to_string()),
                artifact_commands: Vec::new(),
            })
            .expect("stdin is the CLI's own sink");
        assert_eq!(resolved.stdin.as_deref(), Some("s3cret"));
        assert!(
            delivered
                .lock()
                .unwrap()
                .values()
                .any(|value| value == "s3cret"),
            "the run remembers what it delivered"
        );
    }

    #[test]
    fn image_captures_are_withheld_until_the_page_moves_on() {
        let capture = |first: &str| BrowserCliInvocation {
            argv: vec![first.to_string()],
            stdin: None,
            artifact_commands: vec![vec![first.to_string()]],
        };
        assert!(invocation_captures_pixels(&capture("screenshot")));
        assert!(invocation_captures_pixels(&capture("pdf")));
        assert!(
            !invocation_captures_pixels(&capture("snapshot")),
            "text observations run and are scrubbed"
        );
        assert!(
            !invocation_moves_the_page(&capture("click")),
            "a click may be a show-password toggle"
        );
        assert!(!invocation_moves_the_page(&capture("press")));
        assert!(invocation_moves_the_page(&capture("goto")));
        assert!(invocation_moves_the_page(&capture("reload")));
        assert!(!invocation_moves_the_page(&capture("snapshot")));
        assert!(
            invocation_observes_page_text(&capture("snapshot")),
            "a text observation without the value clears it"
        );
        assert!(!invocation_observes_page_text(&capture("click")));
        assert!(
            !invocation_observes_page_text(&capture("get")),
            "a partial read proves nothing about the page"
        );
        let withheld = withheld_capture_result();
        assert!(!withheld.success);
        assert_eq!(
            withheld
                .parsed_json
                .as_ref()
                .and_then(|v| v["status"].as_str()),
            Some("capture_withheld")
        );
        // A value split across fields is recognisable in no text: only commands
        // that act without reading run, and only a navigation clears the level.
        for acts in ["click", "press", "reload", "open", "scroll", "wait"] {
            assert!(
                invocation_only_acts(&capture(acts)),
                "{acts} acts without reading the page"
            );
        }
        for reads in [
            "snapshot",
            "text",
            "html",
            "get",
            "eval",
            "find",
            "screenshot",
            "pdf",
        ] {
            assert!(
                !invocation_only_acts(&capture(reads)),
                "{reads} returns page content"
            );
        }
        let batch = BrowserCliInvocation {
            argv: vec!["batch".into()],
            stdin: None,
            artifact_commands: vec![vec!["click".into(), "@e1".into()], vec!["snapshot".into()]],
        };
        assert!(
            !invocation_only_acts(&batch),
            "a batch that reads anywhere reads"
        );
        let empty = BrowserCliInvocation {
            argv: vec!["help".into()],
            stdin: None,
            artifact_commands: Vec::new(),
        };
        assert!(
            !invocation_only_acts(&empty),
            "an invocation with no known commands is not known to act only"
        );
        let withheld = withheld_page_result();
        assert!(!withheld.success);
        assert!(withheld.stderr.contains("digit by digit"));
    }

    #[test]
    fn truncated_browser_capture_fails_without_returning_partial_content() {
        let mut result = AgentBrowserToolResult {
            success: true,
            stdout: "partial private page".into(),
            stderr: String::new(),
            parsed_json: Some(json!({"partial": true})),
            elapsed_ms: 1,
            stdout_truncated: true,
            stderr_truncated: false,
        };
        reject_truncated_capture(&mut result);
        assert!(!result.success);
        assert!(result.stdout.is_empty());
        assert!(result.parsed_json.is_none());
        assert_eq!(
            result.stderr,
            "browser command output exceeded its configured capture limit"
        );
    }

    /// Batch strings are parsed shell-style by agent-browser, so agents may
    /// not send them: the `commands` matrix is checked token by token.
    #[test]
    fn native_batch_string_mode_is_refused() {
        for args in [
            json!({"args": ["--bail", "open https://example.com", "snapshot -i"]}),
            json!({"args": ["open \"https://example.com\" \"--exec\"utable-path /tmp/x"]}),
            json!({"args": ["open 'fi'le:///etc/hosts"]}),
        ] {
            let error = tool_call_to_cli_args("batch", &args).unwrap_err();
            assert!(error.to_string().contains("`commands`"), "{error}");
        }
        assert_eq!(
            tool_call_to_cli_args(
                "batch",
                &json!({"args": ["--bail"], "commands": [["open", "https://example.com"]]})
            )
            .unwrap(),
            vec!["batch", "--bail"]
        );
    }

    /// A matrix token agent-browser might unquote is checked unquoted too.
    #[test]
    fn quoted_tokens_cannot_hide_an_option_or_a_file_url() {
        for command in [
            json!([["open", "https://example.com", "\"--cdp\"", "9222"]]),
            json!([["open", "https://example.com", "--exec\\utable-path=/tmp/x"]]),
            json!([["open", "'--profile' Default"]]),
            json!([["open", "fi\"le:///etc/hosts"]]),
        ] {
            let fx = crate::magician_v2::execution::goal_workspace_access::test_fixture();
            let refused = crate::magician_v2::execution::goal_workspace_access::with_test_roots(
                vec![fx.root.clone()],
                || {
                    crate::magician_v2::execution::native_executors::ACTING_AGENT
                        .sync_scope(Some("ambassador".to_string()), || {
                            tool_call_to_cli_invocation("batch", &json!({"commands": command}))
                        })
                },
            );
            assert!(refused.is_err(), "{command}");
        }
    }

    #[test]
    fn direct_command_maps_to_exact_cli_argv() {
        assert!(tool_call_to_cli_args("batch", &json!({"args": ["snapshot -i"]})).is_err());

        let argv = tool_call_to_cli_args("click", &json!({"args": ["@e3"]})).expect("click argv");
        // `open` accepts the page as args[0] or as `url`; never as nothing.
        assert_eq!(
            tool_call_to_cli_args("open", &json!({"args": ["https://example.com"]})).unwrap(),
            vec!["open", "https://example.com"]
        );
        assert_eq!(
            tool_call_to_cli_args(
                "open",
                &json!({"url": "https://example.com", "connection_mode": "headless"})
            )
            .unwrap(),
            vec!["open", "https://example.com"]
        );
        assert_eq!(
            tool_call_to_cli_args(
                "open",
                &json!({"args": ["--headed"], "url": "https://example.com"})
            )
            .unwrap(),
            vec!["open", "https://example.com", "--headed"]
        );
        let missing =
            tool_call_to_cli_args("open", &json!({"connection_mode": "headless"})).unwrap_err();
        assert!(
            missing.to_string().contains("needs the page URL"),
            "{missing}"
        );
        assert_eq!(argv, vec!["click", "@e3"]);
    }

    #[test]
    fn structured_batch_uses_json_stdin_with_spacing_injected() {
        // Pin spacing to a known value so the test doesn't depend on the
        // ambient env var.
        let _guard = EnvGuard::set(BATCH_COMMAND_SPACING_ENV, "50");
        let invocation = tool_call_to_cli_invocation(
            "batch",
            &json!({
                "args": ["--bail", "--json"],
                "commands": [
                    ["mouse", "move", "194", "398"],
                    ["mouse", "down"],
                    ["screenshot", "--full", "/tmp/page.png"]
                ]
            }),
        )
        .expect("structured batch invocation");
        assert_eq!(invocation.argv, vec!["batch", "--bail", "--json"]);
        assert_eq!(
            invocation.stdin.as_deref(),
            Some(
                r#"[["mouse","move","194","398"],["wait","50"],["mouse","down"],["wait","50"],["screenshot","--full","/tmp/page.png"]]"#
            ),
            "structured commands should have wait subcommands injected between every consecutive pair before being sent to the CLI"
        );
        assert!(
            invocation.artifact_commands.contains(&vec![
                "screenshot".to_string(),
                "--full".to_string(),
                "/tmp/page.png".to_string()
            ]),
            "structured subcommands should feed artifact detection"
        );
        assert!(
            !invocation
                .artifact_commands
                .iter()
                .any(|cmd| cmd.first().map(String::as_str) == Some("wait")),
            "synthetic wait subcommands must not leak into artifact-detection input"
        );
    }

    #[test]
    fn batch_spacing_skips_when_caller_already_inserted_wait() {
        let spaced = space_batch_commands(
            vec![
                vec!["mouse".into(), "down".into()],
                vec!["wait".into(), "100".into()],
                vec!["mouse".into(), "up".into()],
            ],
            50,
            150,
        );
        // Caller's wait is preserved; no synthetic 50ms wait is added on
        // either side of it.
        assert_eq!(
            spaced,
            vec![
                vec!["mouse".to_string(), "down".to_string()],
                vec!["wait".to_string(), "100".to_string()],
                vec!["mouse".to_string(), "up".to_string()],
            ]
        );
    }

    #[test]
    fn batch_spacing_zero_disables_injection() {
        let original = vec![
            vec!["mouse".into(), "move".into(), "10".into(), "10".into()],
            vec!["mouse".into(), "down".into()],
            vec!["mouse".into(), "up".into()],
        ];
        let spaced = space_batch_commands(original.clone(), 0, 150);
        assert_eq!(spaced, original);
    }

    #[test]
    fn batch_spacing_single_command_is_passthrough() {
        let original = vec![vec!["snapshot".into(), "-i".into()]];
        let spaced = space_batch_commands(original.clone(), 50, 150);
        assert_eq!(spaced, original);
    }

    #[test]
    fn batch_spacing_uses_settle_after_high_level_mutation() {
        // A high-level mutation (click) gets the longer settle gap before the
        // next step, so the page's handlers + re-render land before the read.
        let spaced = space_batch_commands(
            vec![
                vec!["click".into(), "@e1".into()],
                vec!["eval".into(), "document.title".into()],
            ],
            50,
            150,
        );
        assert_eq!(
            spaced,
            vec![
                vec!["click".to_string(), "@e1".to_string()],
                vec!["wait".to_string(), "150".to_string()],
                vec!["eval".to_string(), "document.title".to_string()],
            ],
            "the gap after a click should be the settle value, not the base"
        );

        // Raw mouse micro-steps are gesture fragments and keep the base spacing.
        let mouse = space_batch_commands(
            vec![
                vec!["mouse".into(), "down".into()],
                vec!["mouse".into(), "up".into()],
            ],
            50,
            150,
        );
        assert_eq!(
            mouse[1],
            vec!["wait".to_string(), "50".to_string()],
            "raw mouse fragments keep the base spacing, not the settle"
        );

        // settle_ms is clamped to be at least the base spacing.
        let clamped = space_batch_commands(
            vec![
                vec!["type".into(), "hello".into()],
                vec!["press".into(), "Enter".into()],
            ],
            80,
            10,
        );
        assert_eq!(clamped[1], vec!["wait".to_string(), "80".to_string()]);
    }

    #[test]
    fn batch_spacing_env_override_is_picked_up() {
        let _guard = EnvGuard::set(BATCH_COMMAND_SPACING_ENV, "10");
        assert_eq!(batch_command_spacing_ms(), 10);
        let _guard_zero = EnvGuard::set(BATCH_COMMAND_SPACING_ENV, "0");
        assert_eq!(batch_command_spacing_ms(), 0);
    }

    /// Restore the previous value of an env var on drop. Tests that
    /// mutate process-wide env need this so they don't leak state.
    struct EnvGuard {
        key: &'static str,
        prior: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prior = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, prior }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.prior.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn help_runs_cli_docs_without_browser_connection() {
        assert_eq!(
            help_tool_call_to_cli_args(&json!({})).unwrap(),
            vec!["--help"]
        );
        assert_eq!(
            help_tool_call_to_cli_args(&json!({"command": "click"})).unwrap(),
            vec!["click", "--help"]
        );
        assert_eq!(
            help_tool_call_to_cli_args(&json!({"args": ["skills", "get", "core", "--full"]}))
                .unwrap(),
            vec!["skills", "get", "core", "--full"]
        );
    }

    #[test]
    fn invalid_args_are_rejected_before_spawn() {
        let err = tool_call_to_cli_args("bad command", &json!({"args": ["@e1"]}))
            .expect_err("multi-token command rejected");
        assert!(format!("{err}").contains("one argv token"));

        let err = tool_call_to_cli_args("click", &json!({"args": [1]}))
            .expect_err("non-string arg rejected");
        assert!(format!("{err}").contains("must be a string"));

        let err = tool_call_to_cli_invocation("auth", &json!({"stdin": 1}))
            .expect_err("non-string stdin rejected");
        assert!(format!("{err}").contains("`stdin` must be a string"));

        let err = tool_call_to_cli_invocation(
            "batch",
            &json!({"args": ["get text body"], "commands": [["snapshot", "-i"]]}),
        )
        .expect_err("structured batch rejects positional command strings in args");
        assert!(format!("{err}").contains("only batch options"));

        let err = tool_call_to_cli_invocation(
            "batch",
            &json!({"args": ["--json"], "commands": [["help", "frame", "--help"]]}),
        )
        .expect_err("structured batch rejects Magician-only help tool");
        assert!(format!("{err}").contains("standalone help"));
    }

    #[test]
    fn lightpanda_render_gate_covers_direct_and_batched_pixel_commands() {
        assert_eq!(
            lightpanda_render_only_command(&[vec!["screenshot".into(), "page.png".into()]]),
            Some("screenshot")
        );
        assert_eq!(
            lightpanda_render_only_command(&[
                vec!["batch".into(), "--json".into()],
                vec!["snapshot".into(), "-i".into()],
                vec!["pdf".into(), "page.pdf".into()],
            ]),
            Some("pdf")
        );
        assert_eq!(
            lightpanda_render_only_command(&[
                vec!["snapshot".into(), "-i".into()],
                vec!["get".into(), "text".into(), "body".into()],
            ]),
            None
        );
    }

    #[test]
    fn artifact_detection_uses_exact_cli_args_and_stdout() {
        let commands = vec![
            vec![
                "screenshot".to_string(),
                "--full".to_string(),
                "/tmp/page.png".to_string(),
            ],
            vec!["pdf".to_string(), "/tmp/page.pdf".to_string()],
        ];
        let artifacts = artifacts_for_commands(
            &commands,
            "Saved screenshot: /tmp/other.png\n",
            Some(&json!({"path": "/tmp/from_json.har"})),
        );
        let paths: Vec<String> = artifacts
            .iter()
            .filter_map(|artifact| artifact.effective_path())
            .map(|path| path.display().to_string())
            .collect();

        assert!(paths.contains(&"/tmp/page.png".to_string()));
        assert!(paths.contains(&"/tmp/page.pdf".to_string()));
        assert!(paths.contains(&"/tmp/other.png".to_string()));
        assert!(paths.contains(&"/tmp/from_json.har".to_string()));
    }

    #[test]
    fn stdout_artifact_detection_ignores_page_text_path_tokens() {
        let paths = artifact_paths_from_stdout(
            "Target: http://localhost:5173/tests/sota-tests/18-dual-range-slider.html\n",
        );

        assert!(paths.is_empty());
    }

    #[test]
    fn batch_artifact_detection_inspects_cli_subcommands_and_stdout() {
        let argv = vec![
            "batch".to_string(),
            "--json".to_string(),
            "open https://example.com".to_string(),
            "screenshot --full /tmp/page.png".to_string(),
        ];
        let commands = artifact_command_candidates(&argv);
        let artifacts = artifacts_for_commands(
            &commands,
            "Saved screenshot: /tmp/generated.png\n",
            Some(&json!([{"path": "/tmp/from_batch_json.pdf"}])),
        );
        let paths: Vec<String> = artifacts
            .iter()
            .filter_map(|artifact| artifact.effective_path())
            .map(|path| path.display().to_string())
            .collect();

        assert!(paths.contains(&"/tmp/page.png".to_string()));
        assert!(paths.contains(&"/tmp/generated.png".to_string()));
        assert!(paths.contains(&"/tmp/from_batch_json.pdf".to_string()));
    }

    #[test]
    fn artifact_root_diff_captures_generated_screenshot_without_stdout_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let before = collect_artifact_root_files(temp.path()).expect("snapshot");
        let generated = temp.path().join("agent-browser-generated.png");
        std::fs::write(&generated, b"fake-png").expect("write generated screenshot");

        let artifacts =
            artifacts_created_in_root_since(temp.path(), &before).expect("artifact diff");
        let paths: Vec<String> = artifacts
            .iter()
            .filter_map(|artifact| artifact.effective_path())
            .map(|path| path.display().to_string())
            .collect();

        assert_eq!(paths, vec![generated.display().to_string()]);
        assert_eq!(artifacts[0].kind, "screenshot");
        assert_eq!(artifacts[0].source_tool, "screenshot");
    }
}

/// Goal workspaces and local services through the browser, whose daemon and
/// Chrome run outside every OS box.
#[cfg(test)]
mod local_access_tests {
    use super::*;
    use crate::magician_v2::execution::goal_workspace_access as access;
    use crate::magician_v2::execution::native_executors::ACTING_AGENT;
    use serde_json::json;

    fn invoke(agent: &str, tool: &str, args: Value) -> Result<BrowserCliInvocation> {
        ACTING_AGENT.sync_scope(Some(agent.to_string()), || {
            tool_call_to_cli_invocation(tool, &args)
        })
    }

    #[test]
    fn browser_refuses_file_urls_while_goal_workspaces_exist() {
        let fx = access::test_fixture();
        let goal = fx.root.join("Magican/Goals/X/x.md");
        let url = url::Url::from_file_path(&goal).unwrap().to_string();
        access::with_test_roots(vec![fx.root.clone()], || {
            for agent in ["ambassador", "cmo"] {
                assert!(
                    invoke(agent, "open", json!({"args": [url]})).is_err(),
                    "{agent}"
                );
                assert!(invoke(agent, "open", json!({"args": ["file:///tmp/page.html"]})).is_err());
            }
            // The admin reads every workspace anyway.
            assert!(invoke(access::DEFAULT_ADMIN_AGENT, "open", json!({"args": [url]})).is_ok());
            assert!(invoke(
                "ambassador",
                "goto",
                json!({"url": goal.display().to_string()})
            )
            .is_err());
            assert!(invoke("ambassador", "tab", json!({"args": ["new", url]})).is_err());
        });
        // With no goal workspace, a local page the agent may read opens.
        assert!(invoke(
            "ambassador",
            "open",
            json!({"args": ["file:///tmp/page.html"]})
        )
        .is_ok());
    }

    #[test]
    fn browser_upload_of_a_goal_file_is_refused_for_non_members() {
        let fx = access::test_fixture();
        let goal = fx.root.join("Magican/Goals/X/x.md").display().to_string();
        let secret = fx
            .root
            .join("Magican/Goals/X/secret.md")
            .display()
            .to_string();
        access::with_test_roots(vec![fx.root.clone()], || {
            assert!(invoke("ambassador", "upload", json!({"args": ["@e1", goal]})).is_err());
            assert!(invoke("cmo", "upload", json!({"args": ["@e1", secret]})).is_err());
            assert!(invoke("cmo", "upload", json!({"args": ["@e1", goal]})).is_ok());
            assert!(invoke("ambassador", "state", json!({"args": ["load", goal]})).is_err());
        });
    }

    #[test]
    fn browser_screenshot_into_a_workspace_is_refused_for_non_members() {
        let fx = access::test_fixture();
        let shot = fx.root.join("Magican/shot.png").display().to_string();
        let pdf = fx.root.join("Magican/README.pdf").display().to_string();
        access::with_test_roots(vec![fx.root.clone()], || {
            assert!(invoke("ambassador", "screenshot", json!({"args": [shot]})).is_err());
            assert!(invoke("ambassador", "pdf", json!({"args": [pdf]})).is_err());
            assert!(invoke("cmo", "screenshot", json!({"args": [shot]})).is_ok());
        });
    }

    #[test]
    fn browser_batch_subcommands_are_checked() {
        let fx = access::test_fixture();
        let goal = fx.root.join("Magican/Goals/X/x.md").display().to_string();
        access::with_test_roots(vec![fx.root.clone()], || {
            let matrix = invoke(
                "ambassador",
                "batch",
                json!({"commands": [["open", "https://example.com"], ["upload", "@e1", goal]]}),
            );
            assert!(matrix.is_err());
            let strings = invoke(
                "ambassador",
                "batch",
                json!({"args": ["open file:///etc/hosts"]}),
            );
            assert!(strings.is_err());
        });
    }

    #[test]
    fn http_and_about_blank_navigation_still_pass_but_local_services_do_not() {
        use crate::magician_v2::execution::shell_box::{
            register_loopback_service, register_magician_listener, LoopbackService,
        };
        register_loopback_service(8081, LoopbackService::Supervisor);
        register_loopback_service(3021, LoopbackService::NotesServer);
        register_loopback_service(3003, LoopbackService::Magicutor);
        register_magician_listener(3002);
        for target in [
            "https://example.com",
            "about:blank",
            "localhost:3000",
            "example.com/a",
        ] {
            assert!(
                invoke("ambassador", "open", json!({"args": [target]})).is_ok(),
                "{target}"
            );
        }
        for target in [
            "chrome://settings",
            "view-source:file:///etc/hosts",
            "javascript:alert(1)",
            "http://127.0.0.1:8081/",
            "http://localhost:3021/.fs/Workspaces",
            "http://127.0.0.1:3003/",
        ] {
            assert!(
                invoke(
                    access::DEFAULT_ADMIN_AGENT,
                    "open",
                    json!({"args": [target]})
                )
                .is_err(),
                "{target}"
            );
        }
        // Magician's API is refused only to an agent a workspace is fenced
        // from.
        assert!(invoke(
            access::DEFAULT_ADMIN_AGENT,
            "open",
            json!({"args": ["http://127.0.0.1:3002/"]})
        )
        .is_ok());
        let fx = access::test_fixture();
        access::with_test_roots(vec![fx.root.clone()], || {
            assert!(invoke(
                "ambassador",
                "open",
                json!({"args": ["http://127.0.0.1:3002/"]})
            )
            .is_err());
        });
    }

    /// A fenced agent's browser opens only public pages, whatever the
    /// spelling (a wildcard or trailing-dot name, an IPv4-mapped address, a
    /// LAN or private address, a bare host), never a `data:` or `blob:` page
    /// it could script past every check; the admin is not fenced.
    #[test]
    fn a_fenced_agent_opens_only_public_pages_under_any_spelling() {
        let fx = access::test_fixture();
        access::with_test_roots(vec![fx.root.clone()], || {
            for target in [
                "http://localtest.me:3000/",
                "http://localhost.:3000/",
                "http://[::ffff:127.0.0.1]:3000/",
                "http://[::ffff:7f00:1]:8080/",
                "http://10.0.0.5/",
                "http://192.168.1.1/",
                "http://169.254.169.254/latest/meta-data/",
                "localhost:3000",
                "data:text/html,<script>location='http://127.0.0.1:8081/'</script>",
                "blob:https://example.com/0b1c",
                "http://unresolvable.invalid/",
            ] {
                for agent in ["ambassador", "cmo"] {
                    assert!(
                        invoke(agent, "open", json!({"args": [target]})).is_err(),
                        "{agent} {target}"
                    );
                }
            }
            assert!(invoke("ambassador", "open", json!({"args": ["https://1.1.1.1/"]})).is_ok());
            assert!(invoke(
                access::DEFAULT_ADMIN_AGENT,
                "open",
                json!({"args": ["data:text/html,hi"]})
            )
            .is_ok());
            assert!(invoke(
                access::DEFAULT_ADMIN_AGENT,
                "open",
                json!({"args": ["http://10.0.0.5/"]})
            )
            .is_ok());
        });
    }

    /// The fenced browser's flags proxy loopback too (`<-loopback>` removes
    /// Chrome's implicit loopback bypass) and keep WebRTC inside the proxy.
    #[test]
    fn fenced_browser_flags_send_everything_through_the_strict_listener() {
        let args = fenced_browser_args_for(4243);
        let flags: Vec<&str> = args.split(',').collect();
        assert!(
            flags.contains(&"--proxy-server=http://127.0.0.1:4243"),
            "{args}"
        );
        assert!(flags.contains(&"--proxy-bypass-list=<-loopback>"), "{args}");
        assert!(
            flags.contains(&"--force-webrtc-ip-handling-policy=disable_non_proxied_udp"),
            "{args}"
        );
    }

    /// No agent token may set an option only Magician sets, wherever it
    /// stands (before or after the target, `--flag value` or
    /// `--flag=value`), in a command, a structured batch or a batch string.
    #[test]
    fn magician_only_browser_options_are_refused_in_every_agent_token() {
        for (flag, value) in [
            ("--executable-path", "/tmp/evil"),
            ("--args", "--no-sandbox"),
            ("--proxy", "http://127.0.0.1:1"),
            ("--proxy-bypass", "*"),
            ("--cdp", "9222"),
            ("--auto-connect", "true"),
            ("--profile", "Default"),
            ("--allow-file-access", "true"),
            ("--init-script", "/tmp/x.js"),
            ("--extension", "/tmp/ext"),
            ("--config", "/tmp/c.json"),
            ("--namespace", "other"),
            ("--session", "magician-other"),
            ("--state", "/tmp/state.json"),
            ("--engine", "lightpanda"),
            ("-p", "browserbase"),
        ] {
            let forms = [
                vec![
                    "https://example.com".to_string(),
                    flag.to_string(),
                    value.to_string(),
                ],
                vec![
                    flag.to_string(),
                    value.to_string(),
                    "https://example.com".to_string(),
                ],
                vec!["https://example.com".to_string(), format!("{flag}={value}")],
            ];
            for args in forms {
                for agent in ["ambassador", access::DEFAULT_ADMIN_AGENT] {
                    assert!(
                        invoke(agent, "open", json!({"args": args})).is_err(),
                        "{agent} {args:?}"
                    );
                }
            }
            assert!(
                invoke("ambassador", "snapshot", json!({"args": [flag, value]})).is_err(),
                "{flag}"
            );
            assert!(invoke(
                "ambassador",
                "batch",
                json!({"commands": [["open", "https://example.com", flag, value]]})
            )
            .is_err());
            assert!(invoke(
                "ambassador",
                "batch",
                json!({"args": [format!("open https://example.com {flag} {value}")]})
            )
            .is_err());
        }
        // Per-command options Magician does not own still pass.
        assert!(invoke(
            "ambassador",
            "open",
            json!({"args": ["https://example.com", "--headed"]})
        )
        .is_ok());
        assert!(invoke("ambassador", "snapshot", json!({"args": ["-i", "--json"]})).is_ok());
    }

    #[test]
    fn only_a_scheme_position_names_a_file_url() {
        for text in [
            "file:///etc/hosts",
            "FILE:///etc/hosts",
            "location='file:///etc/hosts'",
            "url=file:/x",
            "view-source:file:///etc/hosts",
            "  file:x",
        ] {
            assert!(names_file_scheme(text), "{text}");
        }
        for text in [
            "profile:[1]",
            "{myfile: 1}",
            "x.file:1",
            "web+file:x",
            "profile: file",
        ] {
            assert!(!names_file_scheme(text), "{text}");
        }
    }

    /// The page a command opens is read from its grammar: a value-taking
    /// option before the target does not hide it; a `file:` token is refused
    /// in any command while a workspace exists; a fenced agent gets no
    /// command that leaves its own browser, no `read` fetch of a URL and no
    /// target that is not a checkable web address; `help` runs no command.
    #[test]
    fn browser_targets_follow_the_command_grammar_and_fenced_agents_stay_in_their_browser() {
        let fx = access::test_fixture();
        access::with_test_roots(vec![fx.root.clone()], || {
            for agent in ["ambassador", "cmo"] {
                for args in [
                    json!({"args": ["--headers", "{\"a\":\"1\"}", "file:///etc/hosts"]}),
                    json!({"args": ["--headers", "{}", "http://127.0.0.1:8081/"]}),
                    json!({"args": ["--timeout", "5", "http://10.0.0.5/"]}),
                    json!({"args": ["http://%%%"]}),
                ] {
                    assert!(
                        invoke(agent, "open", args.clone()).is_err(),
                        "{agent} {args}"
                    );
                }
                assert!(invoke(
                    agent,
                    "eval",
                    json!({"args": ["location='file:///etc/hosts'"]})
                )
                .is_err());
                // Page script whose words merely end in "file" is not a
                // `file:` URL (`profile:` once refused a Reddit read).
                assert!(invoke(
                    agent,
                    "eval",
                    json!({"args": ["JSON.stringify({profile:[...document.querySelectorAll('a[href*=\"/user/\"]')].length, myfile: 1})"]})
                )
                .is_ok());
                assert!(invoke(
                    agent,
                    "cookies",
                    json!({"args": ["set", "--curl", "file:///etc/hosts"]})
                )
                .is_err());
                for (command, args) in [
                    ("connect", json!({"args": ["9222"]})),
                    ("plugin", json!({"args": ["add", "x"]})),
                    ("dashboard", json!({"args": ["start"]})),
                    ("stream", json!({"args": ["enable"]})),
                    ("read", json!({"args": ["https://1.1.1.1/"]})),
                    (
                        "tab",
                        json!({"args": ["new", "--json", "http://localhost:3000/"]}),
                    ),
                    (
                        "record",
                        json!({"args": ["start", "/tmp/r.webm", "http://192.168.1.1/"]}),
                    ),
                ] {
                    assert!(
                        invoke(agent, command, args.clone()).is_err(),
                        "{agent} {command} {args}"
                    );
                }
                assert!(invoke(agent, "read", json!({"args": ["--outline"]})).is_ok());
                assert!(invoke(
                    agent,
                    "open",
                    json!({"args": ["--headers", "{}", "https://1.1.1.1/"]})
                )
                .is_ok());
            }
            // The admin is not fenced.
            assert!(invoke(
                access::DEFAULT_ADMIN_AGENT,
                "read",
                json!({"args": ["https://example.com"]})
            )
            .is_ok());
        });
        for argv in [
            json!({"args": ["open", "file:///etc/passwd"]}),
            json!({"args": ["open", "--executable-path", "x", "--help"]}),
            json!({"args": ["connect", "9222"]}),
            json!({"args": ["skills", "get", "/etc/passwd"]}),
        ] {
            assert!(help_tool_call_to_cli_args(&argv).is_err(), "{argv}");
        }
        assert!(help_tool_call_to_cli_args(&json!({"args": ["open", "--help"]})).is_ok());
    }

    /// A `batch` inside the `commands` matrix is refused for every agent: its
    /// string commands would skip every per-command check.
    #[test]
    fn a_nested_batch_in_the_commands_matrix_is_refused() {
        let fx = access::test_fixture();
        let shot = fx.root.join("Magican/x.png").display().to_string();
        access::with_test_roots(vec![fx.root.clone()], || {
            for commands in [
                json!([["batch", "read http://169.254.169.254/"]]),
                json!([["batch", "connect ws://127.0.0.1:9222"]]),
                json!([
                    ["open", "https://example.com"],
                    ["batch", format!("screenshot {shot}")]
                ]),
            ] {
                for agent in ["cmo", access::DEFAULT_ADMIN_AGENT] {
                    let refused = invoke(agent, "batch", json!({"commands": commands}));
                    assert!(refused.is_err(), "{agent} {commands}");
                }
            }
        });
    }

    /// No agent's browser opens a harness grant channel, under any spelling
    /// of this host.
    #[tokio::test]
    async fn the_browser_never_opens_a_grant_channel() {
        use crate::magician_v2::execution::plane::grant_channel::{forget, open_to};
        let (channel, _guard) = open_to("plt_browser_channel_test", 1, Vec::new())
            .await
            .unwrap();
        for url in [
            format!(
                "http://127.0.0.1:{}/api/magician/v2/plane/mcp",
                channel.port
            ),
            format!("http://[::ffff:127.0.0.1]:{}/", channel.port),
            format!("http://localhost:{}/", channel.port),
        ] {
            for agent in ["ambassador", access::DEFAULT_ADMIN_AGENT] {
                assert!(
                    invoke(agent, "open", json!({"args": [url]})).is_err(),
                    "{agent} {url}"
                );
            }
        }
        forget("plt_browser_channel_test");
    }
}
