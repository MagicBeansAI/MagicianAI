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
| `/god` / `/godoff` typed as the whole message (web composer or any channel bot: Kapso/WhatsApp, Telegram, …) | The same per-chat override as the orb, answered with a short confirmation; the agent does not run on it. |

Settings persist in the live YAML; composer overrides last until backend restart.
Native Chat offers a mode menu; native and web VibeDev expose Super God mode.
Owner login sessions and verified paired owner phones may change modes; phone
chat overrides must belong to the paired workspace. Agent and automation tokens
remain ineligible.

`/god` and `/godoff` (also `/god on|off`, `/god@Bot`, `/magic /god`; trimmed,
case-insensitive) are handled in `POST /chat/sessions/{id}/messages[/stream]`
(`chat_api::god_mode_command_response`, rules in
`magician/src/magician_v2/chat/god_command.rs`). On web the orb's owner rule
applies. On a channel the sender must be the owner by `envoy::is_owner` with
server-derived verification, and the session must have been opened for that
same channel address; a bot token alone is not the owner. Guest
(envoy-bound) and engagement (`eng:`) sessions never change. From anyone who
is not proved the owner the text goes to the agent as an ordinary message,
with no refusal.

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
  working directory; blocked-list commands require an exact invocation approval).
- `http`: a destination the network policy refuses asks (this host, or any
  host), then the call runs unfiltered. A granted destination follows
  redirects only to the same host and port (or, under `public`, to a public
  or `allow_private` IP literal); any other hop returns the 3xx so the next
  request is checked again (`native_executors::granted_redirect_followed`).
  Magician's own runtime HTTP API also asks in God mode; approval permits the
  destination, while the API still requires its normal authentication. Its redirects
  stay on the approved origin. Harness MCP/grant channels and other protected local
  services remain fenced, as do private addresses for goal-workspace-fenced agents.
- File actions: any path, including outside `file_sandbox.allowed_roots`, and
  writes or deletes the file sandbox would refuse. The repo-source and
  runtime-store fences still apply.
  `grep` and `glob` use the same read approval for roots outside the sandbox,
  with the same once/session/always choices; goal-workspace and index fences remain.
- `write_file`, `edit_file` and `apply_patch`: show the staged diff with
  **Allow once / This session / Always / Deny**, scoped to the named files,
  folders or top directories. A matching session/always grant applies later
  edits without another diff approval. Every apply retains revision checks,
  snapshots and rollback. Turning God mode off or redirecting a pending target
  invalidates that edit; after restart it must be restaged. Goal-workspace,
  repo and protected runtime-store fences remain enforced.
- Governed MCP approvals offer once/session/always for one action or the whole
  integration (newly discovered actions included), scoped to its
  endpoint/account and reviewed policy, with God mode on or off. Governed CLI
  tool-access asks can grant the whole package; its argument and filesystem
  rules still apply. Session grants follow the originating chat or root run;
  Settings → God mode → Forget approvals clears always grants. Payment
  checkout keeps its transaction-bound approval.
- Harness engines (Claude Code, Codex, …): `shell` comes off the plane's
  nesting floor (`NEVER_ON_THE_PLANE`) for a live harness turn whose chat or
  run has god mode on, and is listed without a search
  (`plane/grant.rs::god_mode_lifts_floor`). Every call still goes through the
  plane dispatcher and the Decision Engine check. Terminal grants keep the
  floor.

Other actions inside the normal sandbox run as before, under the composer's
Do posture (Ask / Accept).

## What god mode never lifts: goal workspaces

Neither god mode, super god mode nor `file_sandbox.mode: unrestricted` lifts
[goal workspace access](../../features/goal-workspaces.md#who-can-see-a-workspace):
file actions are checked after the override, a fenced agent's shell and
harness stay in the OS box (`shell_box::wrap_workspace_only`) or do not run,
and its `http` tool never gets a private address. Trusted agents may use the
owner's signed-in browser over CDP without God mode; workspace fencing alone
does not forbid that transport. Confined engagement/program runs require an
explicit `cdp` entry in the agent's `browser_transports`; an empty legacy ceiling
does not grant that identity. Trust, transport and browser local-file checks also
apply to cached sessions.

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
service (`request_type: sandbox_override`, `context.god_mode: true`, published
as a `user_request` with wire input type `choice`). Web and mobile render it
as a God-mode authorization in Attention and every workspace chat composer.
The originating chat/run owns the grant regardless of where it is answered. The card
offers "Allow once", then "This session: …" and "Always: …" at each scope that
applies, then "Deny" (also the answer on timeout, 30 min):

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
loops, and a grant covers a line only if it covers every command in it. Only
the exact invocation scope is offered when:

- it matches `shell_sandbox.blocked_command_fragments`;
- it evaluates shell expansions (`$CMD`, `${…}`, `$(…)`, arithmetic or process
  substitutions); malformed or empty syntax is refused before approval;
- it defines a function, or uses `eval`, `source` or `.`;
- an interpreter runs inline code (`bash -c`, `python3 -c`, `node -e`) or reads
  code from stdin (`… | sh`);
- a program runs another command from its arguments (`sudo`, `env`, `xargs`,
  `find -exec`, `timeout`, `ssh`, `nohup`, `hash`, `alias`, …); `command -v`
  / `-V` only looks a name up and runs nothing, and `find`'s own tests and
  patterns (`-name '*.py'`) are not scripts it runs;
- a command is named by a path (`./git`, `/usr/bin/git`, `~/bin/x`);
- it sets a variable anywhere (`PATH=. git`, `X=1; …`, `export`, `declare`,
  `unset`), including a step's own `env`, which the card shows as a prefix
  (`PATH='/tmp/x' git status`);
- it redirects output to a file (`>`, `>>`, `&>`, `>|`) other than
  `/dev/null`, `/dev/stdout`, `/dev/stderr` or another descriptor (`2>&1`).

An exact invocation (a blocked line, a line with stdin, an opaque line, or one
that runs a file) can get a session or always grant only in one shape: the
whole line is a single simple command that runs one script alone, as
`./path/script [args…]` or `bash|sh script [args…]` with no option before
the script, and plain literal words only (printable ASCII, no backslash) (no redirect, pipeline, list,
group, assignment, env prefix, expansion, glob, `~` or quoting beyond plain
single quotes). The script must be a regular text file of at most 16 MiB
outside system folders (`/usr`, `/opt`, `/Library`, … compared
case-insensitively; temp folders allowed), not a native binary, and a direct
run's `#!` may name only bash or sh. Python is never bound, since it
imports sibling files the grant cannot bind: a line that runs one bare
`python`/`python3`/`python3.N` alone (inline `-c`, a quoted heredoc
`python3 - <<'PY'`, or a script; no prefix, pipe, list or expansion outside
the quoted heredoc) offers "Allow once" or "For this chat/run: any `python3`
command" (`shell-interpreter=<program>`, session only, never "Always"). The grant binds the command,
working directory, stdin, blocked status and the SHA-256 of the script; it is
re-checked after the owner answers and before every later run, so a changed
script asks again. Every other exact invocation can only be allowed once.
Plain lines that run no file (`git status`, `npm install`) keep their program
and prefix scopes. Gaps: files the script itself imports or runs are not
bound, the interpreter is found through the service's `PATH`, and the file
is checked by path, so a write between the check and the run (or during it)
is not caught.

Reusable exact-script grants use `shell-invocation=<digest>`; program/prefix
grants cannot cover them. Supplied stdin is fingerprinted, not stored in grants.

A file move asks to write its source as well as its destination.

An approval answered after what it rests on changed is refused and not
recorded: the god mode settings changed, super god mode changed or ended (a
harness launch), or god mode was turned off for that chat or run (shell,
files, network).

Resets: turning `execution.god_mode` on or off forgets every session grant,
every always grant and every chat's override (god mode and auto-approve);
changing auto-approve keeps grants (it is not in their fingerprint). Turning a chat off forgets that
chat's session grants, except super god mode's harness approvals. Settings → "Forget approvals" clears grants without
changing the setting.

## Auto-approve

`execution.god_mode.auto_approve` (default off; Settings → Agent powers) and
each chat's override (the composer's Auto switch beside the Magician orb on
web; the God modes menu on iOS and Android) approve god mode's asks without
asking wherever god mode is on. A run follows its launching chat's choice, else
the default. It records no grant, so turning it off brings the asks back at once.

It still asks (`god_mode::auto_approve_holds_back`; the card adds "Auto-approve
still asks: …"; `context.auto_approve_held_back`) when:

- a shell line matches `shell_sandbox.blocked_command_fragments`;
- a command the line shows deletes or runs as root (`shell_scope::hazard`,
  checked at every depth the reader binds, under runners such as `xargs`,
  `timeout` and `nohup`, inside `find -exec`, `bash -c '…'`, `ssh host '…'`
  and command substitutions): `rm` with `-r`/`-f` or a pattern or expanding
  argument, `find -delete`, `rsync --delete`, `shred`, `dd of=`, `mkfs`,
  `diskutil erase…`, `chmod`/`chown`/`chgrp -R`, `git reset --hard`,
  `git clean` (not `-n`), a forced or deleting `git push`, `git branch -d/-D`,
  `git checkout -- …`, `git restore` of the worktree, `git stash drop/clear`,
  `git rm`; `sudo`, `doas`, `su`, `pkexec`;
- some of what runs is not in the line at all (`ShellReading::hidden`): a
  command or inline code from an expansion, code fed to a shell's input
  (`curl … | sh`), a script chosen by a glob, a line that does not parse;
- an MCP tool's name deletes (`delete`, `remove`, `trash`, `drop`, `purge`,
  `wipe`, `erase`, `destroy`);
- a staged edit deletes a file (status `D`);
- it is a super god mode harness launch, which asks every time by design.

Original-folder edits (VibeDev's review of the real repository) never
auto-approve. A plain `rm file`, a script the agent runs (`./run.sh`,
`python3 x.py`) and a `PATH=` prefix are approved; a script's contents are not
read. Each auto-approval logs `[GOD-MODE] auto-approved` with session and kind.

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

- `GET/PUT /api/magician/v2/settings/god-mode`: `{ enabled }` and/or
  `{ auto_approve }` (PUT) and `{ enabled, auto_approve, always_grants }`.
  Writes the live `magician-config.yaml` and applies at once (also on
  `/settings/magician-config/reload`).
- `DELETE /api/magician/v2/settings/god-mode/grants`: forget every grant.
- `GET/PUT /api/magician/v2/chat/sessions/{session_id}/god-mode`:
  `{ enabled: bool | null }` and/or `{ auto_approve: bool | null }` (null
  follows the default) and `{ enabled, override, default, auto_approve,
  auto_approve_override, auto_approve_default }`. Chat overrides live in
  memory and clear on restart.

All require an owner login session or a verified paired owner device.
