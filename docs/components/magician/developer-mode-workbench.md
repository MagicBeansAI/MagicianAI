# Developer Mode Workbench

Plan: `docs/archive/plans/2026-05-13-developer-mode-workbench.md`. Companion doc: [CLI Delegate Ecosystem](cli-delegate-ecosystem.md) covers operator-launched CLIs and concurrency caps. Agent coding-delegate skills are retired; agents use `run_coding_task`.

Developer Mode is a per-thread layout of the chat surface for watching and
working with a CLI-driven agent. The thread, sessions, messages and event
stream are the same as in chat mode; only the chrome changes.

## Mental model

- **Chat mode (default):** conversation first; tool calls show as cards in the
  message stream.
- **Developer mode:** the thread still owns everything, but the chat canvas is
  replaced by the **Workbench**: an xterm.js terminal, session tabs, a diff
  strip, plan-mode approval cards and a live tool inspector. The floating
  composer writes to the active PTY as stdin, and its secondary row hosts the
  CLI and cwd launcher.

The mode is a per-thread flag (`ui_threads.display_mode`), so threads can differ.
Toggle with cmd/ctrl-shift-D on a thread page (`routes/(app)/t/[name]/+layout.svelte`);
opening a session from `/devsessions` also sets `dev`.

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
│   - HTTP endpoints: catalog / start / stdin / resize / list /      │
│     buffer / close / diff (owner-only)                             │
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

Implemented in the `magician-pty` crate (`magician-pty/src/session.rs`) and
re-exported at `magician/src/magician_v2/execution/interactive_process.rs`. It
wraps `portable-pty` so the **operator** can run a child in a real pseudo-tty
and drive it from the Workbench.

**Operator-only; not an agent tool.** No agent tool, catalog entry or
`ExecutableAction` exposes it, and `interactive_process` is on the
`NEVER_ON_THE_PLANE` floor in `auth/sessions.rs`, so no terminal grant can
include it. Agents that need a coding CLI use `run_coding_task` (see
[CLI Delegate Ecosystem](cli-delegate-ecosystem.md)). The only production caller
is `magician-api/src/interactive_process_api.rs`.

| Function | Behavior |
|---|---|
| `start_session_with` | Spawns a PTY session from a `StartSpec` (`program, args, working_dir, env, env_remove, rows, cols, host, concurrency_limit, scope`). Returns `session_id` and the initial output burst (up to 750 ms). Used by `POST /interactive-sessions`. `start_session` is the direct-host, no-scrub form for tests and library callers. |
| `write_session` | Raw stdin bytes; the caller controls line endings. Used by `POST /interactive-sessions/{id}/stdin`. |
| `resize_session` | Sets the window size (`master.resize`, so the child gets `SIGWINCH`). Used by `POST /interactive-sessions/{id}/resize`. |
| `adopt_tmux_sessions` | Re-attaches a scope's tmux-hosted sessions from a previous process under their original ids. Called from `GET /interactive-sessions` and before `start`. |
| `close_session_with_transcript` | Graceful terminate and reap, persisting the transcript. Used by `DELETE /interactive-sessions/{id}`. |
| `read_session`, `press_key` | Draining read and named-key press (up/down/enter/…/`bytes:<hex>`). Library API with no production caller. |

Sessions are scope-isolated (`registry_for_scope(principal, workspace)`). On
registry teardown the session's `Drop` kills the PTY child: the program for a
direct-hosted session, only the tmux client for a tmux-hosted one (see
[Session host and restart survival](#session-host-and-restart-survival)).

**Owner-only.** A terminal runs programs as the host user, so every endpoint in
`interactive_process_api.rs` (directory picker and CLI catalog included) passes
`require_terminal_operator`: the caller must be the install's owner (the first
login identity, as enrollment uses) by session or API token. Bot tokens,
terminal (`plt_`) grants and other members' sessions get
`403 terminal_owner_only`, even in their own scope. With no stamped identity,
access is allowed only while the install has no identity at all (open-mode
bootstrap).

**CLI catalog and concurrency cap.** `interactive_process.operator_cli_programs`
in `magician-config.yaml` lists the CLIs the launcher offers (`claude`, `codex`,
`grok`, `agy`, `opencode`, `zsh` in the checked-in config); there is no
built-in catalog. It is also the server allowlist: `POST /interactive-sessions`
refuses any unlisted `program`. A bare name resolves against the child's `PATH`,
then `~/.local/bin`, `~/.grok/bin`, `~/.claude/local`, `~/.bun/bin`,
`/opt/homebrew/bin` and `/usr/local/bin`, since a launchd- or supervisor-started
service has none of those on `PATH` (`resolve_program_path`). Pi is deliberately
not listed: it is the internal engine behind `run_coding_task`.
`interactive_process.max_concurrent_per_program` caps live sessions per binary
process-wide (six each for the coding CLIs by default). It is a runaway guard,
not a license meter. The start handler reads the live config on every request,
so a reload applies without a restart.

### Live PTY rendering

The PTY reader thread writes every chunk to:

1. the session's in-memory buffer (drained by `read_session`);
2. the realtime bus as `RuntimeTransportEvent::InteractivePtyChunk { session_id, principal, workspace, program, bytes_b64, timestamp_ms }`.

With `display_mode: dev`, `InteractiveTerminalPane` subscribes to the event hub
(`eventHub.subscribe`, filtered to its `session_id`) and feeds bytes to xterm.js,
so colors, cursor moves and alternate-screen TUIs render correctly. The
broadcast is fan-out; chat-mode users never see it.

Refresh and navigation use a separate non-draining replay buffer.
`GET /api/magician/v2/interactive-sessions/{id}/buffer` returns the bounded byte
history (4 MiB) with absolute offsets; the pane resets xterm, writes the
snapshot, then applies live chunks that arrived meanwhile, deduplicated by
offset. Reconnecting a UI never consumes output.

A dropped chunk corrupts a full-screen TUI until it redraws, so the pane
re-hydrates whenever it may have missed bytes:

- the hub reports a resync (`hub.resync` after dropped events for a lagging
  client, or a reconnect that could not resume, including a server restart);
- a live chunk starts past the rendered offset;
- offsets restart at 0 (a re-attached session begins a new buffer under the same
  id);
- the previous fetch failed, on the next live chunk.

### User take-over

`POST /api/magician/v2/interactive-sessions/{id}/stdin` (base64 body) writes the
operator's keystrokes. The client (`writeInteractiveSessionStdin`) keeps one
request per session in flight and batches everything typed meanwhile into the
next, so keys never arrive out of order and a slow tunnel costs one round trip,
not one per key. A failed request prints one warning in the terminal.

### Terminal size

The PTY starts at the launcher's 40x140, then follows the viewer: the pane posts
its xterm grid to `POST /api/magician/v2/interactive-sessions/{id}/resize`
(`{rows, cols}`, bounded to 500x1000) after mounting, on every xterm resize
(debounced 120 ms), when its tab becomes active and when the terminal takes
focus. The CLI gets `SIGWINCH` and redraws. With several viewers, the last to
resize or focus sets the size; a hidden pane never sends one.

### Operator CLI launcher

The launcher sits in the composer's secondary row, replacing the Do/Plan switch,
attachment button and dispatch hint. Its dropdown loads
`GET /api/magician/v2/interactive-sessions/cli-runtimes`, backed by
`interactive_process.operator_cli_programs`. Agent definitions that still expose
`code_generate_*_cli` tools only annotate matching entries; they add none.
**Start** calls `POST /api/magician/v2/interactive-sessions` with `program`,
optional `working_dir` and a 40x140 size the pane then replaces (see
[Terminal size](#terminal-size)). The backend starts the session in the scoped
registry with `PtyBroadcastConfig`, so output streams at once and keystrokes go
through `/stdin`.

Dev mode has one input mode, terminal focus. Typing in the xterm pane sends raw
keys. Text sent from the composer goes to the active PTY followed by Enter, and
focus returns to the terminal; multi-line text is wrapped as a bracketed paste
(`ESC[200~ … ESC[201~`) so an inner newline is not an early Enter
(`composerSubmissionBytes`). It never creates a chat message; switch back to
Chat mode for that.

The CWD control has a server-side directory picker,
`GET /api/magician/v2/filesystem/directories`. It lists directories on the
machine running Magician, not the browser device, so it works the same from any
client. It returns directories plus Parent/Home/Root/Current shortcuts and fills
`working_dir`.

The workbench renders inside the chat canvas; the transcript stays mounted
behind it so switching modes keeps scroll and state. New sessions become the
active tab and get focus.

When the launcher passes a `ui_thread_id`, the session stores it, every
`InteractivePtyChunk` carries it, and the Workbench filters both
`/interactive-sessions` hydration and live chunks to the current thread, so one
thread never shows another thread's session.

VibeDev's Preview panel reuses this substrate: it polls
`GET /api/magician/v2/interactive-sessions`, reads replay buffers, extracts local
HTTP(S) URLs printed by dev servers and embeds the chosen one in the VibeDev
Agents surface. It never starts, stops or supervises processes.

**Session hygiene.** `GET /api/magician/v2/interactive-sessions` returns program,
thread, cwd, creation time, last input and output times, replay offsets and
process state. Tabs show CLI, id and age, with full metadata in the tooltip, and
mark a session stale after 15 minutes without input or output. The header has
**Close all** for the thread; the launcher's **Sessions** popover lists every
live PTY in the workspace so orphans can be closed. `/devsessions` (Ops dropdown
and command palette) shows the same inventory as a 1320px dashboard, following
live chunks; clicking a tile switches its thread to Developer Mode with
`dev_session=<session_id>` so that tab is selected.

The launcher is operator control only. It does not change how agents choose
tools or force coding agents through a chosen CLI.

### Diff strip

Each session captures its `working_dir` at spawn.
`GET /api/magician/v2/interactive-sessions/{id}/diff` returns the changed files
there. `DiffStrip` polls every 3 s in dev mode, lists files with `+N/-M` counts
and expands inline diffs. Revert shows the `git checkout` command; there is
deliberately no destructive server endpoint.

The handler always runs three `git` processes, however many files changed:
`git diff --name-status HEAD` for status codes, one whole-tree
`git diff --unified=3 HEAD` for the patch, and `git ls-files --others` for
untracked files. `split_unified_diff_by_file` splits the patch on column-0
`diff --git` / `diff --cc` headers, and chunks match the `--name-status` entries
by position (same diff machinery, same rename detection). Per-file
`git diff -- <path>` calls are avoided because at a 3 s poll they scale forks
with branch size. Since the whole-tree patch sees both sides of a **rename**, a
pure rename reports `+0/-0` and a rename with edits reports only the edit.
Untracked files are rendered as a synthetic new-file diff, capped at 256 KB and
skipped when binary.

### Plan-mode gate

When the thread's `plan_mode` flag is on, the agentic executor sends every
non-read-only action through the tool-authorization escalation (the flow
`tool_authorization_policy: ask_user` uses). The question carries a
`[plan-mode]` prefix so `PlanModePane` can render it apart from other prompts.

`is_read_only_action` (in `execution/agentic/types.rs`) treats as read-only:

- `FileAction::Read` / `List` / `Exists`;
- `HttpAction` with `GET` / `HEAD` / `OPTIONS`;
- `DuckDbAction` (analytical SELECTs in practice);
- a pack capability on the `is_parallelizable_read_only_pack` allowlist
  (`query_known_resource`, the `list_*` and `get_*` introspection packs,
  `search_memory`, `grep`, `glob`, `read_file`, `web_search`, `web_fetch`,
  `content_search`, `content_read`, `inspect_agent`,
  `find_agents_for_capability`, `system_status`, `time_math`,
  `read_program_state`, `read_trace`).

Everything else (bash, browser, edit, pack writes) trips the gate.

### Live tool inspector

`LiveToolInspector` follows `AgenticStepStarted` / `AgenticStepCompleted` /
`AgenticStepFailed` and shows in-flight tools with elapsed time. Cancel calls the
existing execution-cancellation endpoint.

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
    - grok
    - agy
    - opencode
    - zsh
  max_concurrent_per_program:
    claude: 6
    codex: 6
    grok: 6
    agy: 6
    opencode: 6
    # Programs without an entry have no limit (bash, git, gh, etc.)
  session_host: auto   # auto | tmux | direct
```

The thread record holds the toggles (`ui_threads.display_mode = 'chat' | 'dev'`,
`ui_threads.plan_mode = false | true`), added by additive
`ALTER TABLE ADD COLUMN IF NOT EXISTS` migrations, so existing scopes upgrade
safely.

## Session host and restart survival

`interactive_process.session_host` decides what owns a terminal's program:

| Mode | Behaviour |
|---|---|
| `auto` (default) | tmux when a `tmux` binary is found (on `PATH`, then `/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin`, `/bin`), else `direct` |
| `tmux` | tmux, and `start` answers `503` when tmux is missing |
| `direct` | the program is the PTY child and dies with Magician |

With tmux, `start` creates a detached session `mgc-<session_id>` on a
**private** tmux server and puts a tmux client on the PTY. When Magician exits
(crash, `replace-restart-magician`, upgrade), only the client dies; the program
keeps running. The next process re-attaches: `GET /interactive-sessions` (and
`start`, before counting the cap) calls `adopt_tmux_sessions` at most once per
10 s per scope, attaching a new client to each session that belongs to the
caller's `(principal, workspace)` and is not registered. It keeps the original
id, program, thread, cwd and creation time, so the tab reconnects to the same id
and re-hydrates. `close` runs `tmux kill-session`, ending the program; Ctrl-D is
not sent, because the CLI would see it as a keystroke.

- **Private server.** The socket is `-L magician-wb-<hash of the storage root>`,
  so the operator's own tmux is untouched and two Magician instances on one
  machine never adopt each other's terminals.
- **Server config** (`TMUX_SERVER_CONFIG`, written to
  `<storage_root>/interactive_sessions/tmux.conf`, loaded with `-f` and
  re-sourced on every start): no prefix key and no status line, so it behaves
  like the bare CLI (Claude Code binds Ctrl-B); `escape-time 0`;
  `default-terminal tmux-256color` with truecolor (`RGB`) passed to the outer
  `xterm-256color`; `smcup@:rmcup@` keeps tmux out of the outer alternate screen
  so scrolled-off output lands in the browser's scrollback;
  `window-size latest`; `history-limit 50000`.
- **Metadata** lives on the tmux session as user options
  (`@magician_session_id`, `@magician_principal`, `@magician_workspace`,
  `@magician_program`, `@magician_thread`, `@magician_cwd`,
  `@magician_created_ms`).
- The program is resolved against the `PATH` it will receive before any session
  exists, so a missing CLI is refused with "not found on PATH". It runs as
  `/usr/bin/env <path> <args…>` so tmux execs it directly, not through
  `default-shell -c`.
- tmux runs with `-u` (UTF-8 whatever the service locale) and without
  `TMUX`/`TMUX_PANE`, so a Magician started inside tmux can still attach.
- A tmux-hosted session reports `persistent: true` in
  `GET /interactive-sessions`. Its exit code is the client's, not the program's.
- **Scrollback survives the restart.** Adoption seeds the new replay buffer
  with the pane's history (`capture-pane -p -e -J -S -5000`, colours kept,
  capped at half the buffer), then a screen of blank lines so the new client's
  redraw lands below it.

## Shell and worktrees

`zsh` is in `operator_cli_programs`. A shell started with no args (`zsh`,
`bash`, `fish`, `sh`) runs as a **login shell** (`-l`), so the operator's
`.zprofile`/`.zshrc`, `PATH` and toolchains load: the environment scrub removes
Magician's, and the login shell adds theirs.

The launcher's **Worktree** button calls
`POST /api/magician/v2/filesystem/worktrees` `{repo_path, branch, base?}`
(owner-only) for the repo in the cwd field. It runs `git worktree add` at
`<repo-root>-<branch slug>` beside the repository (a new branch from `base`,
default `HEAD`, or an existing branch), refuses an existing path or invalid
branch name (`git check-ref-format`), and switches the cwd to the new path. One
terminal per worktree keeps parallel CLI sessions out of each other's files.

## Terminal environment

An operator terminal gets the operator's environment, not the service's. The
start handler removes from the child (and, for tmux, from the server it starts,
whose global environment every program inherits):

- every key `load_runtime_env_files` added from Magician's runtime
  `.env.development` / `.env` (recorded at boot via
  `set_runtime_injected_env_keys`). These are the service's credentials; left
  in, `claude` would bill `ANTHROPIC_API_KEY` instead of the operator's
  subscription and `gh` would act as Magician's `GITHUB_TOKEN`;
- `ALWAYS_SCRUBBED_TERMINAL_ENV_KEYS`: the provider keys `ANTHROPIC_API_KEY`,
  `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_API_KEY`, `XAI_API_KEY`, `OPENAI_API_KEY`,
  `CODEX_API_KEY`, `GEMINI_API_KEY` and `GOOGLE_API_KEY` (an env key overrides the CLI's own sign-in
  in `~/.claude`, `~/.grok/auth.json`, `~/.codex`, and this also covers keys the
  service inherited from its launching shell, which the `.env` scrub cannot
  see), plus the build knobs the `Makefile` exports (`CARGO_TARGET_DIR`,
  `RUSTUP_TOOLCHAIN`, `RUSTC_WRAPPER`, `RUSTFLAGS`, `MAKEFLAGS`, `MAKELEVEL`, …),
  which would otherwise send a worktree's builds to the main checkout's target
  dir and make every `make` a sub-make of a dead jobserver.

`PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `TMPDIR` and the locale are never
removed. `TERM=xterm-256color` and `COLORTERM=truecolor` are set unless the
request's `env` names them; request `env` is applied last.

## On-disk artifacts

When a session closes (`DELETE /interactive-sessions/{id}`), its captured output
(with a header naming the replay offsets and whether the 4 MiB replay buffer
truncated it) is written to
`<storage root>/scopes/<principal>/<workspace>/interactive_sessions/<session_id>.log`
(default storage root `magician_data_v3`), so the thread can replay what the CLI
did after the session is gone. Empty output writes nothing; a failed mkdir or
write logs a warning and skips the file, and the close still succeeds.

## Bash stdin

The `bash` lane takes an optional `stdin` parameter, so `cat <<< 'y'` style
flows need no heredoc when the answer is known up front.

## Related

- [CLI Delegate Ecosystem](cli-delegate-ecosystem.md)
