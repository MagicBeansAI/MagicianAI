# God mode

God mode gives trusted agents unrestricted `shell`, files and network, so a
chat or run can be pointed at a GitHub repo or an MCP URL and clone, install
and use it on the fly. Anything the normal sandbox would refuse asks the owner
first. Code: `magician/src/magician_v2/god_mode.rs`.

## Where it is on

| Control | Scope |
|---|---|
| `execution.god_mode.enabled` (Settings → god mode panel, top of the page) | The default for every chat, and for runs no chat started (scheduled, triggers, background). |
| The Magician orb in the composer (click to toggle) | One chat: overrides the default for that chat, its harness turns, and every run it starts (runs carry `chat_session_id`). |

God mode is for chat, agent runs and background operations. VibeDev is not
affected. Its coding engines use their own native tools, not Magician's, so
there is nothing for god mode to unrestrict; they stay in Magician's box
(see "VibeDev" below). Only super god mode lifts it.

Only trusted agents qualify: built-in and local trust levels
(`god_mode::trust_allows`). Untrusted and reviewed agents keep the normal
sandbox whatever the setting.

## The normal sandbox it lifts

Without god mode, shell commands outside a coding run are OS-enforced
(`shell_sandbox.os_enforce`, `execution/shell_box.rs`): writes only under the
working-dir roots, temp and tool caches; secret stores unreadable; nested agent
CLIs not executable; network per `execution.network_policy` (`public`: the
internet through Magician's egress proxy, loopback allowed directly, private
LAN/metadata blocked).
The `http` tool applies the same network policy. Design:
2026-10-13-hard-shell-sandbox-design.md.

## What changes

- `shell`: with the OS box on, every command a grant does not cover asks
  before it runs, then runs outside the box (no OS profile, no proxy). A
  refusal inside the box happens mid-command, too late to ask, so god mode
  asks first. With the box off, only what the string checks refuse asks (any
  working directory; blocked-list commands become askable, once only).
- `http`: a destination the network policy refuses asks (this host, or any
  host), then the call runs unfiltered. A granted destination follows
  redirects only to the same host and port (or, under `public`, to a public
  or `allow_private` IP literal); any other hop returns the 3xx so the next
  request is checked again (`native_executors::granted_redirect_followed`).
- File actions: any path, including outside `file_sandbox.allowed_roots`, and
  writes or deletes the file sandbox would refuse. The repo-source and
  runtime-store fences still apply.
- Harness engines (Claude Code, Codex, …): `shell` comes off the plane's
  nesting floor (`NEVER_ON_THE_PLANE`) for a live harness turn whose chat or
  run has god mode on, and is listed without a search
  (`plane/grant.rs::god_mode_lifts_floor`). Every call still goes through the
  plane dispatcher and the Decision Engine check. Terminal grants keep the
  floor.

Actions inside the normal sandbox run exactly as before, under the composer's
Do posture (Ask / Accept).

## VibeDev

Normally every VibeDev engine process (Claude Code, Codex, Pi, Grok,
Antigravity) runs inside Magician's box (`coding.box_harness`, default on;
`coding_engine::harness_command`, `shell_box::wrap_harness`). The box is the
same OS profile as the shell box, applied to the engine and everything it
runs:
- writes go only to the engine's workspace copy (the shadow), its own state
  directories (`~/.claude`, `~/.codex`, `~/.pi`, `~/.gemini`, …), temp and
  build caches (download caches only: `~/.cargo/bin`, `~/.pyenv/shims`,
  `~/.deno` and other executable dirs stay unwritable);
- Grok writes only `sessions/` and `logs/` and its state files in Magician's
  own Grok home; that home's config, hooks and plugins stay read-only and are
  emptied on every prepare, and `HOME` points at it for a contained launch;
  a restricted Grok turn refuses to start without that home;
- never writable even under a writable root: the coding run's real repo
  (`HarnessBox::deny_write`), agent install dirs and the engine's own file and
  package, `~/.claude/settings.json` / `settings.local.json` (hooks);
- secret stores are unreadable, except the login keychain for the engines
  that sign in through it (Claude Code and Antigravity,
  `coding_engine::keychain_signin`): hidden, they cannot save a refreshed
  token, and macOS asks the owner about a missing keychain; unix sockets
  outside the writable roots
  (ssh-agent, Docker) are closed and `SSH_AUTH_SOCK`, `DOCKER_HOST`, … are
  removed (Linux: env removal only);
- other agent CLIs are blocked by name (`.js`/`.mjs`/`.cjs` too), by npm
  package and by install dir, on or off `PATH` (the engine itself is exempt);
- the internet is reached through the egress proxy, with loopback direct
  except Magician's own API port (`shell_box::register_magician_listener`;
  macOS only).

Discuss turns keep the workspace copy unwritable.

God mode does not apply to a VibeDev run at all: the Build
coordinator (`coding_coordinator_run`), any VibeDev run including a Discuss
coordinator and its engineers (`vibedev_run`, from the task's `vibedev` tag or
thread, kept across a pause), and any agent that itself holds
`run_coding_task`, `apply_code_proposal` or `run_project_checks`
(`agentic::types::is_vibedev_coding_context`) get no god session, in the loop
or on the plane (`plane_god_session`), and `god_mode::current_session` is None
inside any coding context. Every plane call of such a run's harness turn runs
in the coding context (`plane::grant::plane_call_is_coding`). Super god mode launches the engine with no
fence at all (below). `coding.box_harness: false` falls back to the coding
profile alone (the real repo read-only), for rollback.

macOS refuses a nested `sandbox_apply`. So whenever Magician fences an engine
(box or coding profile), it stands in for the engine's own sandbox:
- Codex runs `danger-full-access`. Measured 2026-10-14: its `workspace-write`
  commands exited 71 under the coding profile.
- Grok runs `--sandbox off`. Its `workspace` profile refused to start.

## Super god mode (debug)

`execution.super_god_mode` is set only from Settings, in the god mode panel:
"On for 1 hour" (`until_ms`; it turns itself off) or "On until turned off".
While it is on (`god_mode::super_active`), a harness may start with no
restrictions, but each launch asks the owner first with the same card as god
mode (`GodAction::UnrestrictedHarness`, `god_mode::approve_unrestricted_launch`):
"Allow once", "This session" or "Always", for this engine on this repo
(VibeDev: `harness=<engine>@<repo>`), this engine anywhere
(`harness=<engine>`), or any harness (`harness*`). The card goes to the
chat or run the harness serves, whether or not god mode is on there. Denied,
or timed out, the harness starts restricted as usual. Super god mode does not
turn god mode on, and changes nothing about Magician's own tools: whether they
ask is still god mode's decision. Set it from Settings, or from the ⚡ toggle
in the VibeDev composer (`SuperGodModeToggle.svelte`).

Approved:

- Chat and agent-run harnesses (`plane::engines`, `super_god_unrestricted`)
  keep Magician's bridge (MCP config, grant) and drop their restrictions:
  - Claude Code: no `--tools ""`, `--strict-mcp-config` or
    `--setting-sources ""`, so it gets its native tools and the owner's
    settings and MCP servers.
  - Grok: no `--tools ""`, `--disallowed-tools`, `--no-subagents` or
    `--disable-web-search`.
  - Codex (exec and app-server): `danger-full-access`, every feature on, web
    search allowed.
  - Antigravity: no `--sandbox`.
  - Pi: its own tools, extensions, skills and context files.
  - Each launch logs `[SUPER-GOD] harness launched with no restrictions`.
- VibeDev engines launch with no fence (`HarnessFence::Unrestricted`): no box,
  no coding profile, and none of their own sandboxes. Codex runs
  `danger-full-access` and Grok `--sandbox off` without `--disable-web-search`
  or `--no-subagents`. Antigravity runs without `--sandbox`. Claude Code loads
  the owner's settings and MCP servers and every tool (no
  `--strict-mcp-config`, `--setting-sources ""`, `--safe-mode`, `--no-chrome`,
  `--disable-slash-commands` or `--disallowedTools`), and its init isolation
  check stands aside. Each launch logs `[SUPER-GOD] engine launched with no
  fence`.
- A red banner with "Turn off" shows in chat, tasks (`/tasks`, `/t/…`),
  VibeDev (`/vibe`) and Settings (`SuperGodModeBanner.svelte`, one shared poll).

VibeDev offers any installed, signed-in engine, whatever its isolation qualification says (`discovery::super_god_runs`), but skips qualification only for a turn the owner approved to launch unrestricted (`run_coding_task::super_god_skips_qualification`); a denied or timed-out card leaves the turn fenced and runs only a qualified engine. An unqualified Codex runs the owner's default model from `$CODEX_HOME/config.toml` (else `~/.codex`). Grok uses the owner's own Grok home rather than Magician's clean one.

Launch approvals ask in the run when there is one, keyed by its root run (`run:<root execution id>`, shared with the runs it delegates to), and in the chat only for a chat turn (`god_mode::launch_session`). A VibeDev launch also needs the run's agent to be trusted (`god_mode::with_run_trust`, scoped at the run seam and on every plane call); none scoped means refused.

Turning super god mode off stops every harness that started unrestricted: at once when turned off, within five seconds when its hour runs out (`god_mode::unrestricted_cancel`, `until_super_revoked`). Only that turn is cancelled, through a child of the run's or chat's own cancellation; the run or chat goes on, and its next harness asks again and starts restricted. A VibeDev engine stopped this way reports that super god mode ended, and nothing it wrote is staged. The next Codex plane turn, follow-ups included, goes back to Codex's own read-only sandbox (`super_active` is checked at every turn start). A plane Codex never inherits the VibeDev fence: without its own approval it keeps `read-only`.

Grants and chat overrides are kept.

API: `GET/PUT /api/magician/v2/settings/super-god-mode`. PUT takes
`{ "enabled": true, "duration": "1h" | "forever" }` or `{ "enabled": false }`;
both return `{ active, enabled, until_ms, remaining_secs }`.

## Approvals

Anything the normal sandbox refuses asks the owner through the user-request
service (`request_type: sandbox_override`, `context.god_mode: true`, rendered
as a `choice`: the web UI shows `sandbox_override` input only for agentic and
escalation sources, and user requests publish as `user_request`). The card
offers "Allow once", then "This session: …" and "Always: …" at each scope that
applies, then "Deny" (also the answer on timeout, 300 s):

| Scope | Shell | Files |
|---|---|---|
| Narrowest | exactly this command, every argument as written | this file |
| Middle | `<program> <first argument> …` (any further arguments; offered when the first argument is a literal word, not an option) | anything under its folder |
| Widest | any use of the program | anything under its top directory (`~/projects`, `/opt`) |

A scope that reads the same as a narrower one is left out. "This session"
lasts for the chat session (`chat:<id>`) or run (`run:<root execution id>`), in
memory. A run's session is its delegation tree's root (`god_mode::session_run`):
answered for a coordinator, it covers the engineers it delegates to, while
each question still shows in the run that asks; "always" for the principal and workspace, in
`$MAGICIAN_ROOT_DIR/god_mode_grants.json`, stamped with the settings
fingerprint. Patterns: `shell=<command>`, `shell^<program> <arg>`,
`shell*<program>`, `file=<path>`, `dir=<directory>` (path components, not
string prefixes).

Shell lines are read with a real Bash parser (`god_mode/shell_scope.rs`,
tree-sitter-bash): every simple command is extracted, including inside
command and process substitutions, pipelines, `&&`/`;` lists, subshells and
loops, and a grant covers a line only if it covers every command in it. A line
is "allow once" only, asked every time, when:

- it matches `shell_sandbox.blocked_command_fragments`;
- it does not parse cleanly, or a command name comes from an expansion
  (`$CMD …`);
- it defines a function, or uses `eval`, `source` or `.`;
- an interpreter runs inline code (`bash -c`, `python3 -c`, `node -e`) or reads
  code from stdin (`… | sh`);
- a program runs another command from its arguments (`sudo`, `env`, `xargs`,
  `find -exec`, `timeout`, `ssh`, `nohup`, `hash`, `alias`, …);
- a command is named by a path (`./git`, `/usr/bin/git`, `~/bin/x`);
- it sets a variable anywhere (`PATH=. git`, `X=1; …`, `export`, `declare`,
  `unset`), including a step's own `env`, which the card shows as a prefix
  (`PATH='/tmp/x' git status`);
- it redirects output to a file (`>`, `>>`, `&>`, `>|`) other than
  `/dev/null`, `/dev/stdout`, `/dev/stderr` or another descriptor (`2>&1`).

A file move asks to write its source as well as its destination.

An approval answered after what it rests on changed is refused and not
recorded: the god mode settings changed, super god mode changed or ended (a
harness launch), or god mode was turned off for that chat or run (shell,
files, network).

Resets: changing `execution.god_mode` forgets every session grant, every
always grant and every chat's override. Turning a chat off forgets that
chat's session grants, except super god mode's harness approvals. Settings → "Forget approvals" clears grants without
changing the setting.

## Wiring

- Session: `god_mode::with_session` sets the `GodSession` task-local at the
  run seam (`executor.rs`, carried across hops by `CapturedRunTaskLocals`)
  and at plane dispatch (`plane_god_session`). A chat's in-process compiled
  tool call supplies one from its invocation context; chat runs compiled
  tools in process only for built-in and local agents.
- Enforcement: `execute_bash_action` and `execute_file_action`
  (`native_executors.rs`) ask through `god_mode::authorize` when the sandbox
  refuses; the run's own sandbox preflights stand aside while god mode is
  active. The shell provider's `CommandGuard` defers to the same check.
- Approver: the boot `UserRequestService` (`god_mode::install_approver`).

## API

- `GET/PUT /api/magician/v2/settings/god-mode`: `{ enabled }` (PUT) and
  `{ enabled, always_grants }`. Writes the live `magician-config.yaml` and
  applies at once (also on `/settings/magician-config/reload`).
- `DELETE /api/magician/v2/settings/god-mode/grants`: forget every grant.
- `GET/PUT /api/magician/v2/chat/sessions/{session_id}/god-mode`:
  `{ enabled: bool | null }` (null follows the default) and
  `{ enabled, override, default }`.

All are session-bearer only (owner authority).
