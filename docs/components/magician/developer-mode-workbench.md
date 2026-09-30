# Developer Mode Workbench

Plan: `docs/archive/plans/2026-05-13-developer-mode-workbench.md`. Companion doc: [CLI Delegate Ecosystem](cli-delegate-ecosystem.md) covers operator-launched CLIs and concurrency caps. Agent coding-delegate skills are retired; agents use `run_coding_task`.

Developer Mode is a per-thread layout variant on the existing chat surface that turns Magician into a "watch + collaborate with a CLI-driven agent" environment. The thread, sessions, messages, and event stream stay exactly as they are in chat mode; only the chrome around the chat changes.

## Mental model

- **Chat mode (default):** conversation-first. Tool calls show up as cards in the message stream.
- **Developer mode:** the conversation is still the owning thread, but the visible chat canvas is replaced by the **Workbench**. The floating composer becomes terminal-focused: its textarea/send button writes to the active PTY session as stdin, while the secondary controls row hosts CLI/cwd launcher controls. The workbench hosts an xterm.js terminal, multi-session tabs, a diff strip, plan-mode approval cards, and a live tool inspector.

The mode is a per-thread flag (`ui_threads.display_mode`), so different threads can be in different modes simultaneously. Toggle via the `ContextPill` pill (`chat ↔ dev`) or `cmd-shift-D`.

## Architecture

```text
┌────────────────────────────────────────────────────────────────────┐
│ Same backend                                                       │
│   - chat store, message stream, event bus, agent runtime,          │
│     skills, tasks, artifacts — unchanged.                          │
│                                                                    │
│ New primitives (additive)                                          │
│   - interactive_process module (PTY-backed long-lived sessions)    │
│   - RuntimeTransportEvent::InteractivePtyChunk (live byte stream)  │
│   - InteractiveSessionRegistry (per-scope live-session map)        │
│   - HTTP endpoints: catalog / start / stdin / list / buffer / close / diff │
│                                                                    │
│ New UI (additive, lazy-loaded in dev mode only)                    │
│   - FloatingComposer(dev) → DevWorkbenchLauncher                   │
│   - WorkbenchColumn      → InteractiveTerminalTabs                 │
│                       → InteractiveTerminalPane (xterm.js)         │
│                       → DiffStrip                                  │
│                       → PlanModePane                               │
│                       → LiveToolInspector                          │
└────────────────────────────────────────────────────────────────────┘
```

### `interactive_process` primitive

Implemented in the `magician-pty` crate (`magician-pty/src/session.rs`), re-exported at `magician/src/magician_v2/execution/interactive_process.rs`. Wraps `portable-pty` so the **operator** can spawn a child inside a real pseudo-tty and drive it from the Workbench.

**Operator-only; not an agent tool.** No agent tool, catalog entry or `ExecutableAction` exposes it, and `interactive_process` is on the `NEVER_ON_THE_PLANE` floor in `auth/sessions.rs`, so a terminal grant can never include it. Agents that need a coding CLI use `run_coding_task` (see [CLI Delegate Ecosystem](cli-delegate-ecosystem.md)). The only production caller is the HTTP surface in `magician-api/src/interactive_process_api.rs`.

| Function | Behavior |
|---|---|
| `start_session` | Spawns a PTY-backed child (`program, args, working_dir, env, rows, cols`). Returns `session_id` + initial output burst (up to 750 ms). Called by `POST /interactive-sessions`. |
| `write_session` | Raw stdin bytes; caller controls line endings. Called by `POST /interactive-sessions/{id}/stdin`. |
| `close_session_with_transcript` | Graceful terminate + reap, persisting the transcript. Called by `DELETE /interactive-sessions/{id}`. |
| `read_session`, `press_key` | Draining read and named-key press (up/down/enter/…/`bytes:<hex>`). Library API with no production caller today. |

Sessions are scope-isolated (`registry_for_scope(principal, workspace)`). On registry teardown the session's `Drop` impl kills any still-alive child so leaks can't survive an execution.

**Operator CLI catalog and per-program concurrency cap.** `interactive_process.operator_cli_programs` in `magician-config.yaml` controls which operator-startable CLIs appear in the Developer Mode / VibeDev Workbench dropdown. The checked-in config currently lists `claude`, `codex`, `agy`, and `opencode`; code does not provide a baked-in catalog. Pi is deliberately not listed: Pi is the internal coding-engine adapter behind `run_coding_task`, not an operator CLI launcher option. `interactive_process.max_concurrent_per_program` caps live sessions per binary process-wide (one each for the four operator CLIs: they consume license / quota slots that don't multiplex). `start_session` rejects when the cap is hit.

### Live PTY rendering

The PTY reader thread writes every chunk to two places simultaneously:

1. The session's in-memory buffer (drained by `read_session`).
2. The realtime event bus as `RuntimeTransportEvent::InteractivePtyChunk { session_id, principal, workspace, program, bytes_b64, timestamp_ms }`.

When the thread has `display_mode: dev`, the UI's `InteractiveTerminalPane` subscribes to the SSE stream filtered by `session_id` and feeds bytes into xterm.js — colors, cursor positioning, alternate-screen TUIs all render correctly. Chat-mode users never see the event variant; the broadcast is fan-out, not request-response.

Refresh/navigation recovery uses a separate non-draining replay buffer. `GET /api/magician/v2/interactive-sessions/{id}/buffer` returns the bounded PTY byte history plus absolute byte offsets; the terminal pane hydrates that snapshot before flushing live chunks. The draining `read_session` buffer is left untouched, so reconnecting a UI never consumes output.

### User take-over

`POST /api/magician/v2/interactive-sessions/{id}/stdin` (base64-bytes body) writes the operator's keystrokes into the session. The pane's `🔒 lock-to-agent` toggle silently drops user keystrokes when the user wants to watch the CLI's own agent work uninterrupted; the `⌨ user typing` badge flashes for 5 s after each keystroke.

### Operator CLI launcher

Developer Mode also exposes a direct launcher for operator-started CLI sessions. The launcher lives in the floating composer's secondary controls row, replacing the normal Do/Plan switch, attachment button, and dispatch hint. The dropdown loads the server-owned catalog from `GET /api/magician/v2/interactive-sessions/cli-runtimes`, which is backed by `interactive_process.operator_cli_programs`. Loaded agent definitions that still expose tools named `code_generate_*_cli` only annotate matching configured entries with metadata; they do not add extra dropdown runtimes. Selecting a CLI and pressing **Start** calls `POST /api/magician/v2/interactive-sessions` with `program`, optional `working_dir`, and a 40x140 PTY size. The backend starts a session in the scoped `interactive_process` registry and wires `PtyBroadcastConfig`, so output immediately streams into the xterm pane and user keystrokes flow through the existing `/stdin` endpoint.

In Dev mode there is one foreground interaction mode: terminal focus. Typing directly into the xterm pane writes raw keystrokes to the active PTY. Typing into the floating composer sends the composed text to the active PTY followed by Enter, then re-focuses the terminal. It does not create a normal chat/orchestrator message; switch the thread back to Chat mode when the next input should go through Magician's chat runtime.

The CWD control includes a server-side directory picker backed by `GET /api/magician/v2/filesystem/directories`. This deliberately lists directories on the machine/container running Magician, not the browser device, so it works consistently from macOS, Linux, iOS, and tunneled deployments. The picker returns directory-only entries plus Parent/Home/Root/Current shortcuts and fills the `working_dir` field used by `interactive_process`.

The workbench renders inside the chat canvas. The transcript remains mounted behind it so mode switches preserve scroll/state, but pointer and keyboard focus are terminal-first: newly started sessions become the active tab and the terminal pane is focused automatically, and composer sends return focus to the active xterm.

PTY sessions are thread-linked when the launcher provides a `ui_thread_id`. The start endpoint stores that id on the live session, the realtime `InteractivePtyChunk` payload carries it, and the Workbench filters both `/interactive-sessions` hydration and live chunks to the current thread. That prevents a Dev-mode workbench in `#general` from showing a CLI session started in another thread.

VibeDev's first Preview panel reuses this same live-session substrate. It polls
`GET /api/magician/v2/interactive-sessions`, reads non-draining replay buffers
for candidate sessions, extracts local HTTP(S) URLs printed by dev servers, and
embeds the selected URL inline in the VibeDev Agents surface. The preview bridge
does not start, stop, or supervise processes; those lifecycle responsibilities
are not the bridge's.

Live-session hygiene is visible in the same surface. `GET /api/magician/v2/interactive-sessions` returns program, thread, cwd, creation time, last input/output times, replay offsets, and process state. The tab strip shows CLI/id/age, exposes the full metadata in the tooltip, and marks sessions stale after 15 minutes without input or output. The workbench header has **Close all** for the current thread, while the launcher **Sessions** popover lists all live PTYs in the current workspace so orphaned sessions in other threads can be found and closed. `/devsessions` renders the same live PTY inventory as a 1320px operator dashboard, is reachable from the Ops dropdown and command palette, hydrates from the session-list endpoint, follows live PTY chunk events, refreshes periodically, and clicking a tile switches the owning thread to Developer Mode with `dev_session=<session_id>` so the workbench selects that exact session tab after reconnect.

This launcher is UI/operator control only. Autonomous agents still choose their own tools; the direct launcher does not change worker-agent tool routing or force coding agents through a manually selected CLI.

### Diff strip

Each `interactive_process` session captures its `working_dir` at spawn. `GET /api/magician/v2/interactive-sessions/{id}/diff` returns the changed files in that directory. The frontend `DiffStrip` polls every 3 s in dev mode, lists changed files with `+N/-M` counts, and expands inline diffs with green/red line backgrounds. Revert surfaces the `git checkout` command (intentional — there is no destructive server-side endpoint).

**Three `git` spawns, regardless of how many files changed.** The handler runs `git diff --name-status HEAD` for the status codes, one whole-tree `git diff --unified=3 HEAD` for the patch, and `git ls-files --others` for untracked files. The whole-tree patch is the concatenation of the per-file patches, so `split_unified_diff_by_file` splits it on column-0 `diff --git` / `diff --cc` headers and the chunks are consumed positionally against the `--name-status` listing (both commands run the same diff machinery with the same rename detection, so entry *i* is chunk *i*). Per-file `git diff -- <path>` spawns are avoided: at a 3 s poll they scale forks with branch size on the blocking pool. Because the whole-tree patch sees both sides of a **rename**, git emits its rename chunk, so a pure rename reports `+0/-0` and a rename-with-edits reports only the edit (a per-path pathspec would hide the deletion and show the whole file as new).

Untracked files are unchanged: they are still listed by `git ls-files` and rendered as a synthetic new-file diff, capped at 256 KB and skipped when binary.

### Plan-mode gate

When the thread's `plan_mode` flag is on, the agentic executor routes every non-read-only action through the existing tool-authorization escalation surface (the same flow `tool_authorization_policy: ask_user` uses). The question gets a `[plan-mode]` prefix so the UI's `PlanModePane` can recognize and render it separately from generic authorization prompts.

`is_read_only_action` (in `execution/agentic/types.rs`) classifies an action as read-only when it's:

- `FileAction::Read` / `List` / `Exists`
- `HttpAction` with method `GET` / `HEAD` / `OPTIONS`
- `DuckDbAction` (analytical SELECTs in practice)
- A pack capability from a small allowlist: `query_known_resource`, `list_*`, `get_task_details`, `search_memory`, etc.

Everything else (bash, browser, edit, pack writes) trips the gate.

### Live tool inspector

`LiveToolInspector` subscribes to `AgenticStepStarted` / `AgenticStepCompleted` / `AgenticStepFailed` and tracks in-flight tools with elapsed time. The cancel button calls the existing execution-cancellation endpoint.

## Data flow summary

```text
Operator starts `claude` from the Workbench launcher
   (POST /interactive-sessions)
   ▼
start_session opens PTY, spawns child, returns session_id + initial burst
   ▼
Reader thread streams bytes
   ├─→ output_buffer (read_session)
   └─→ RuntimeTransportEvent::InteractivePtyChunk (for the UI)
   ▼
UI's InteractiveTerminalPane writes bytes to xterm.js
User can also:
   - Type into the pane → POST /interactive-sessions/{id}/stdin → write_session
   - Watch diffs poll in via DiffStrip
   - Approve or reject plan-mode escalations
   - Cancel via LiveToolInspector
```

## Configuration

```yaml
# magician-config.yaml
interactive_process:
  operator_cli_programs:
    - claude
    - codex
    - agy
    - opencode
  max_concurrent_per_program:
    claude: 1
    codex: 1
    agy: 1
    opencode: 1
    # Programs without an entry have no limit (bash, git, gh, etc.)
```

The thread-level toggle lives on the thread record (`ui_threads.display_mode = 'chat' | 'dev'`, `ui_threads.plan_mode = false | true`). Both columns are nullable-with-default-aware via additive `ALTER TABLE ADD COLUMN IF NOT EXISTS` migrations so upgrade is safe for existing scopes.

## On-disk artifacts

When a session closes (`DELETE /interactive-sessions/{id}`), the captured PTY output is best-effort persisted to `magician_data_v3/scopes/<principal>/<workspace>/interactive_sessions/<session_id>.log`. Lets the thread re-open later and replay what the CLI did, even if the session itself is long gone.

## Bash stdin

The `bash` lane takes an optional `stdin` parameter so simple `cat <<< 'y'` style flows don't need a heredoc — appropriate when the answer is known up front and stdin is the only thing the CLI reads.

## Related

- [CLI Delegate Ecosystem](cli-delegate-ecosystem.md)
