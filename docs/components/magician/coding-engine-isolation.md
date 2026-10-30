# Coding-engine isolation (repo deny-fence + OS-FS sandbox)

Keeps a coding agent (or any sandboxed subprocess) from mutating the **live magician repo source
tree** while it works in a per-scope shadow workspace. The default
`storage_path` (`magician_data_v3`) nests the scope sandbox **inside** the repo, so a subprocess
could otherwise walk up to the live `.git`.

## Original project folders

Work location is independent of normal / Super god permissions. A project's
`trust: original` selects its existing folder and current branch, including
uncommitted files; `sandboxed` keeps the persistent copy and `trusted` keeps
an isolated branch. Admission stores `work_location` in the dispatch plan and
project run link; follow-ups inherit it. Original runs have separate records
under `coding_engine/original_runs`, outside clone cleanup and branch actions.
Same-folder runs share the repository admission lock.

For original runs every engine's OS fence holds the checkout read-only,
including under Super god mode and during Discuss. Build turns receive the
per-run Citizen `edit_original` capability and bundled `original_repo_edit.py`
helper. The route authenticates its run grant and private loopback channel in
the handler, like the other Citizen endpoints, instead of using a login token.
The broker stages exact text preimages/replacements, shows the shared
diff/grant card, then applies only the approved transaction after rechecking
path identity and file contents. Deny, expired run tokens, symlinks, hard links,
traversal and stale preimages fail closed. Discuss receives no edit session.
Allow once applies that revision; session grants cover the run chain; always
grants are scoped to the principal, workspace and canonical repository, separate
from chat god-mode grants. No clone, proposal, automatic commit or merge occurs.
Applied transaction history supplies the run's Changes view and completion facts.
An OS sandbox and the live approval channel are required.

## Automatic + scoped to coding runs (no operator flag)

There is no env flag to arm isolation. A spawn or file action is isolated only when both hold:

1. **Coding context active.** `coding_engine::coding_context_active()` reads the `tokio` task-local
   `CODING_RUN_CONTEXT`. `execute_agentically` sets it for the whole loop of an agent that holds a
   **directly-granted** coding capability (`run_coding_task` / `apply_code_proposal` /
   `run_project_checks`, `providing_agent_id.is_none()`); it reaches that agent's shell/file tools and
   its Pi and check-runner spawns (same task, no `tokio::spawn` boundary). General agents and skills
   never set it and are **unaffected**; the check runs FIRST so they skip the launcher probe. The
   **engineering-manager is deliberately NOT flagged**: its inherited tools carry
   `providing_agent_id = Some(engineer)`, so it only coordinates (its own dugite/git ops must stay
   writable); the engineer's own loop arms isolation when it runs the coding tool.
2. **Launcher works.** `coding_engine::os_sandbox_available()` is **default-on** with a one-time
   self-test. macOS runs `sandbox-exec` on a no-op. Linux takes `bwrap` only from `/usr/bin`, `/bin`
   or `/usr/local/bin`, root-owned and not group/world-writable (never from `PATH`), and runs the
   production argv on a scratch layout: a write into the listed root lands, one beside it fails, and
   `/proc` is readable (`bwrap_confines_writes`).

If the launcher is missing or the probe fails, shell/check spawns **fail open** (run unsandboxed).
Kill-switch: `MAGICIAN_CODING_OS_SANDBOX=0`. **Engine turns do not fail open:** Codex/Grok/Claude/Agy
require `require_outer_fence()`; a Pi coding turn requires `require_pi_sandbox()` and refuses to spawn
when `os_sandbox_will_wrap()` is false, unless `coding.pi_allow_unsandboxed: true` on a host that is
its own sandbox (container without user namespaces). Plane-harness Pi turns are not gated.
`run_coding_task` scopes every engine turn with `with_coding_context(true)`, so the gate holds even
when the dispatcher is not a coding agent.

The SBPL builder, vault-deny constants and path escaping are macOS-or-test only. The Linux argv
builder (`BwrapPolicy` → `bwrap_args`) is pure and compiled for tests too, so its mount order is
pinned on every platform (`bwrap_model` replays the mounts).

## The fence pair

`coding_engine::live_repo_source_fence() -> Option<(repo_source_root, sandbox_base)>` is the
process-global source of truth, armed at startup (`magician.rs` → `set_live_repo_source_root` +
`set_live_storage_base`), `Some` only when the launcher CWD has a `.git`. It is **optional for the OS
sandbox**: on a packaged install (`None`) the gate still wraps with the real-repo write deny and
credential read denies, minus the live-repo pair; `require_outer_fence()` still requires it.
`sandbox_base` is the resolved `config.storage_path` (default `<repo>/magician_data_v3`), which stays
WRITABLE (scope data and shadow workspace).

- `sandbox_base` must be a **strict proper subpath** of the repo root; a degenerate `storage_path`
  (`.`/`..`/repo root) falls back to the default.
- The base is leniently canonicalized (nearest existing ancestor + tail), so symlinked fresh deploys
  compare like-for-like.

## The layers

| Layer | Where | Armed | Effect |
|---|---|---|---|
| **OS-FS sandbox (THE GATE)** | `os_sandbox_wrap` / `os_sandbox_command` at both native bash spawns, the CLI-template dispatcher, Pi, the check-runner | coding context + launcher | macOS `sandbox-exec` SBPL (source checkout: `deny file-write*` repo, `allow` sandbox_base; always a TRAILING `deny file-write*` for the real repo plus credential read denies; last-match-wins) / Linux `bwrap` (`--die-with-parent --unshare-pid`, fresh `/dev` and `/proc`; allow-list: `--ro-bind / /` then roots, else `--bind / /` with `--ro-bind` repo and `--bind` sandbox_base; then `--ro-bind` real repo, then hidden credentials). SUBPROCESS only (in-process `tokio::fs` writes: file-action fence). **FAIL OPEN** without coding context or launcher, except engine turns. |
| **Tier-1 real-repo write fence** | trailing rule in `os_sandbox_wrap`, fed by `CODING_RUN_REAL_REPO` (`run_coding_task` via `with_coding_real_repo`) | coding context + launcher + real repo scoped | Real repo READ-ONLY for the Pi turn; Pi writes only its shadow CWD (apply-to-real happens later in `apply_code_proposal`). Closes the external/absolute-`repo_path` gap. |
| **File-action fence** | `native_executors::validate_file_action_repo_fence` | coding context | Blocks mutating `files`/`delegation_files` (Write/Append/Delete/CreateDir + Copy-dest + Move-src&dest) into the repo source. Coding-scoped so general in-repo `data/` (prompt-store) writes keep working. |
| **Shell working_dir fence** | `native_executors::validate_shell_action_hard` | always-on for sandboxed modes; Unrestricted only in a coding context | Rejects an EXPLICIT `working_dir` in the repo source (`shell working_dir=<repo>`). |
| **Repo-binding fence** | `coding_engine::resolve_coding_repo_binding` | always-on | Rejects a `repo_path` in the repo source. |
| **`GIT_CEILING_DIRECTORIES`** | bash spawns (`sandbox_base`), Pi (`scope_root`), CLI dispatcher (`workdirs_root`) | always-on (additive) | Stops git walking up to the live `.git`. |

Shared predicate `reject_repo_source_tree(candidate, repo_source_root, sandbox_base)`: reject when the
repo is a real git repo AND `candidate.starts_with(repo)` AND `!candidate.starts_with(sandbox_base)`.

### Tier-1 real-repo write fence + integrity backstop

The repo-source deny protects only the **magician** tree; without Tier-1 Pi could edit an external
real repo while its shadow stays pristine, **inverting** the captured diff.

- **Scope.** `run_coding_task` wraps the Pi turn in
  `with_coding_real_repo(repo_binding.real_path, run_turn(..))`; `os_sandbox_wrap` reads
  `coding_run_real_repo()`. `None` (non-coding spawns, check/shell leaf executors) changes nothing.
- **macOS.** A TRAILING `(deny file-write* (subpath <real_path>))` after the `allow default` / `deny`
  repo-source / `allow` sandbox_base rules overrides both earlier allows.
- **Linux.** `--ro-bind <real_path> <real_path>` after every writable bind (an external repo is
  simply remounted read-only).
- **Degenerate-shadow guard.** SKIPPED when the shadow NESTS under the real repo
  (`shadow_workspace_root.starts_with(real_path)`), else it would deny the shadow CWD and brick the
  run; the backstop still covers it.
- **Integrity backstop.** `real_repo_fingerprint` (`git status --porcelain`) runs before and after
  the turn; a change (a write escaped the shadow) logs a warning and emits `coding.integrity_warning`
  (**warn, not reject**; the proposal still stages).
- **Prompt.** `append_coding_context` names the CWD (the shadow) as the project, asks for relative
  paths, and shows the `repo_path` hint ONLY when workspace-relative.

### Credential read denies

The macOS profile denies reads no coding task needs:
`(deny file-read* (subpath "$HOME/Library/Keychains"))`, and by filename (no scope-root knowledge)
`(deny file-read* (regex #"/(provisioned_secrets|captured_secrets|mcp_oauth)\.vault$"))` and
`(deny file-read* (regex #"/secret_audit\.jsonl$"))`. The pure `macos_sandbox_profile` builds it;
tests pin rule order after `(allow default)` and run it live under `sandbox-exec`. The read denies
share the gate and kill-switch. Linux hides the vault folders and the session bus (no keychain).

- **Keychain exception** (owner decision 2026-10-09). Claude Code and Antigravity keep their login
  in the keychain (else "Not logged in · Please run /login"; Antigravity's browser OAuth times out). Their spawns pass `SandboxAllowances { keychain_read: true }`
  (`os_sandbox_command_with`), dropping only the keychain deny; items still decrypt via `securityd`
  per-item access lists. Pi, Codex and Grok keep the deny.
- **Nested sandboxes.** macOS refuses `sandbox_apply` inside any restrictive sandbox (only a bare
  `(allow default)` outer profile permits it), so engine
  seatbelts cannot run here: Codex fails with `sandbox-exec: sandbox_apply: Operation not permitted`
  (exit 71) in `codex exec` and the app server, Antigravity's `--sandbox` retries unsandboxed, Grok refuses to start ("could not apply
  the 'workspace' sandbox profile").
- **No `(deny process-info*)`, deliberately:** `KERN_PROCARGS2` stays readable under it (and under
  `process-info-pidinfo` or a `sysctl-read` deny on `kern.procargs*`), while `/bin/ps` and `pgrep`
  cannot `execvp`. Same-uid env leakage is closed by delivery (`stdin` over `env`) or a uid boundary.
- SBPL `subpath` matches the **resolved** path (`/var/folders/…` → `/private/var/folders/…`), so
  `os_sandbox_wrap` canonicalizes every fenced path (keychain, repo-source, shadow, real-repo) first.

### Write allow-list

`coding.sandbox_write_allowlist` (default on; macOS and Linux). `run_coding_task` scopes the turn's
roots (`with_coding_write_roots`: workspace, Pi's session folder, build caches, owner-allowed
folders). The scope root is excluded: it holds the project record and approval answers, so writing
it would widen the sandbox. Project-settings folders (`sandbox_write_paths`, build cache paths) are
rechecked each turn (`admissible_write_root`): absolute, off the never-allow list, not inside or
holding Magician's runtime root or live source (`guarded_magician_paths`) bar `coding_engine`.

**macOS.** `(allow default)(deny file-write*)` plus `/dev/null`, `/dev/tty*`, `/dev/fd/*`, each root,
the temp area (`temp_write_root`: per-user `/private/var/folders/<a>/<b>/T` opens its parent; other
`$TMPDIR` only itself), `/private/tmp` (Linux: `/tmp`, `/var/tmp`), package caches
(`~/.cargo/registry`, `~/.cargo/git`, `~/.rustup`, `~/.npm`, `~/.cache`, `~/Library/Caches`,
`~/.gradle`, `~/.m2`, `~/.hex`, `~/.mix`, `~/go`, `~/.pnpm-store`, `~/.yarn`, `~/.bun`) and engine
state (`engine_state_paths`: `~/.pi`, `~/.claude` + `~/.claude.json*`, `~/.codex`, `~/.gemini`,
`~/.grok`). The live source is re-denied after the roots (only roots in its storage base re-allowed),
so an allowed ancestor (`~/Development`) never reopens it; the real-repo deny stays last. Refusals:
`Operation not permitted`.

**Linux.** Mounts: `--ro-bind / /`, each root `--bind-try` (parents first), live source `--ro-bind`
with storage-base roots re-bound, real repo `--ro-bind`, then each `<root>/scopes/*/*/secrets` under an
empty read-only tmpfs and `$XDG_RUNTIME_DIR/bus` and `MAGICIAN_KEYRING_PASSWORD_FILE` under
`/dev/null`. `DBUS_SESSION_BUS_ADDRESS` is dropped (Secret Service holds the vault key). Missing roots
are skipped, so state folders and roots are created before the spawn. A `X*` entry binds only `X` (bwrap has
no prefixes): `~/.claude.json` is writable in place, but `~/.claude.json.lock` cannot be created (unverified whether Claude Code needs it). Refusals:
`Read-only file system`. The network is shared on purpose.

**Engine sandboxes off** under the list (`os_write_allowlist_will_wrap`): Codex `danger-full-access`
thread / `dangerFullAccess` turn policy (overlays and attestation unchanged), Grok `--sandbox off`,
Antigravity without `--sandbox`. `harness_own_sandbox_off` covers the list and the box alike.

**With the box** (`coding.box_harness`, the default): `harness_box` adds the same roots
(`coding_write_roots`) to the box's writable set, so build caches and owner-allowed folders stay
writable. Without the box, `harness_command` falls back to this profile with each engine's
`engine_allowances` (its state paths, plus keychain read for Claude Code and Antigravity).

**Discuss turns** (plan-only) are fenced by the list alone (engine sandboxes off too), which holds
only Pi's session folder (`coding_turn_write_roots`; no workspace, working dir, build cache or
owner-allowed folder) plus temp, caches and engine state: they read the repo but cannot write it,
and refused writes raise no approval.

**Linux status.** Unit-tested on macOS only; never run on a Linux host.
Hosts restricting unprivileged user namespaces (Ubuntu 24.04+ AppArmor, default Docker seccomp) fail
the self-test, and engine turns fail closed.

### Owner approval for refused writes

`sandbox-exec` denials are not logged, so `run_coding_task`'s event sink scans each raw frame
for `Operation not permitted` / `Read-only file system` / `EPERM` / `EROFS` beside an absolute path
(`coding_engine::write_denials::denied_paths_in`; either order, any case, so zsh's
`operation not permitted: /path` matches; cargo's
``failed to … `/path` … Caused by: … (os error 30)`` across lines). Codex, Claude, Grok and Antigravity
strip `raw` to a stub (no native ids, no tool output), so their adapters scan the native tool
payload and attach only the refused paths (`raw.refused_writes`,
`write_denials::attach_refused_writes`).

After the turn paths collapse to nearest existing folders (`approvable_dirs`), minus the write roots,
real repo, Magician source and the never-allow list (filesystem and home roots, `~/.ssh`, `~/.gnupg`,
`~/.aws`, `~/.kube`, `~/.docker`, `~/.netrc`, `~/.git-credentials`, `~/.password-store`,
`~/.config/gh`, `~/.config/systemd`, `~/.config/autostart`, `~/.local/bin`,
`~/.local/share/keyrings`, `~/.cargo/bin`, the keychain, LaunchAgents, Preferences, system folders
such as `/proc`, `/sys`, `/run`, `/var/lib`) and any folder holding one (`~/.config`); for a holder,
the folder being created is offered instead (`~/.config/newtool`), but not when the nearest folder
is home itself (`~/.zshenv`) or the new folder is itself never-allow.

Remaining paths emit `coding.sandbox_write_blocked`, add
`sandbox_write_blocked { paths, approval_request_id, note }` to the result, and ask the owner once
(`vibedev.sandbox_write_approval`: *Allow for this project* / *Don't allow*; not allowed after 7
days), on Linux too.

- At the project's next coordinator or coding turn, allowed folders are saved (`sandbox_write_paths`,
  `vibedev::sandbox_approvals`) and join the write roots.
- Each answer applies once (`applied_sandbox_approvals`), so a folder the owner removes is not
  re-added from history; setting the list in project settings marks earlier answers applied
  (`sandbox_approvals::mark_answers_applied`). Settings list the folders (remove, or add by hand).
- The coordinator's goal gets a `Coding sandbox (owner-allowed folders):` block saying a refused
  write there is resolved (so a follow-up reruns the step); the refused step is never retried
  automatically.
- `PATCH /vibedev/projects/{id}` `sandbox_write_paths` is owner-only and validated like build cache
  paths (never-allow list, Magician's folders).

## Shadow workspace

`coding_engine::coding_shadow_root(scope_root, real_path)` owns the shadow location
(`<base>/coding_engine/worktrees/<key>`; default base the scope root). `MAGICIAN_CODING_SANDBOX_ROOT=<dir>`
moves the multi-GB shadow out of the data tree; a target **inside the repo but outside the storage
base** is rejected (warn + fall back), since it would desync GIT_CEILING and the OS-sandbox allow rule.

## Shadow admission lock (concurrent same-repo runs)

The shadow, keyed by `persistent_shadow_key(repo_path)`, is mutated from **four** sites:
`run_coding_task`, `run_project_checks`, and two `vibedev_api` handlers (preview-start and the check
endpoint's `project_shadow_root`). `coding_engine::shadow_admission_lock(shadow_key) -> Arc<Mutex<()>>`
(`shadow_lock.rs`) gives each key one process-global async mutex held via `.lock_owned().await`:
same-repo work **serializes**; distinct repos run in parallel.

- `run_coding_task`: sync → Pi turn → byte-diff capture (autopilot fan-out shares it).
- `run_project_checks`: shadow-prepare and the check run.
- `vibedev_api` handlers: create path only (double-checked `!exists()`), so reads never wait on a run.

In-process only (an `fs2` advisory lock would make it cross-process).

## Trusted repo mode (no shadow)

Plan: `docs/archive/plans/2026-10-07-vibedev-trusted-repo-mode.md`.
A `trust: trusted` VibeDev project runs each chain in a **`git clone --shared`** at
`<coding sandbox root>/coding_engine/runs/<root_task_id>` on branch `vibedev/<slug>-<root8>`
(`vibedev::trusted_workspace`; record `<root_task_id>.json`). `resolve_trusted_run_workspace` decides
from server records only (the task's fenced `VibeDev project:` line and the project store, never the
model's `repo_path`); a chain keeps its root's mode.

- **Workspace.** The clone is repo and working dir: no shadow sync, `stage_result = false`, lock per
  chain (`trusted:<root>`), so chains in one repo run in parallel. The clone sits under the storage
  base (writable); `with_coding_real_repo` and the fingerprint target the **owner's repo**
  (`fenced_real_repo`), so the engine still cannot write it. History via the clone's `.git` plus read-only `objects/info/alternates`;
  attachments in `.cache/magician/` (excluded via `.git/info/exclude`).
- **Prompt.** Commit; never push, add remotes, switch branches or rewrite base history.
- **After each non-plan turn** (host-side): leftovers committed (`--no-verify`), `refs/heads/<branch>`
  fetched into the owner's repo (non-forced; `HEAD`, index, working tree untouched). Message
  (`trusted_turn_commit_message`): subject = first plain summary line (else the request), ≤ 72 chars;
  body = summary; trailer `VibeDev-Task: <id>`.
- **Reporting.** `coding.branch_updated` and `trusted_branch` give branch, base, head, commits ahead,
  files changed, clean state, merged/PR and errors. The reason states them
  (`trusted_workspace::branch_fact_sentences`) and carries `git diff base..HEAD`, capped at 16 KB on
  a file boundary (`trusted_workspace::branch_diff`), as claim-check evidence.
- **No proposal.** A committing turn reports `committed: true`, `no_change: false` (tool result and
  `coding.completed`). The coordinator gets a **Trusted project (git branch mode)** block
  (`artifact_v2::service::vibedev_change_delivery_block`, once per run): no `CodeChangeProposal`, no
  diff approval, `trusted_branch` is the evidence; the owner lands it from the Branch tab. The block
  follows dispatch's `trusted_workspace::effective_trust_mode`: a falling-back run gets **Trusted
  project (sandboxed run)** instead, and its result carries `trusted_fallback`.
- **Merge and PR.** `merge_into_base` moves `base_sha` to the merged head (later `commits_ahead`,
  diffs and rewinds count from it); a later commit clears
  `merged` (`record_turn`). The PR route always pushes and returns an open PR's link.
- **Checks** (`POST /vibedev/projects/{id}/check`, `task_id` = chain root) run in the clone with
  `CARGO_TARGET_DIR` = the shared build dir, no side-ref checkpoint (commits are checkpoints).
  **Verification**: coding events carry the clone as `repo_path`, so the gate classifies the run
  `AutopilotSelfApplied` and verifies the clone.
- **Concurrency.** `coding.trusted_max_concurrent_runs` (default 4, `0` = no cap, read at the first
  trusted turn) bounds trusted turns process-wide; waiters emit `coding.queued`.
- **Sweep.** At most hourly, a starting trusted turn reaps clones idle 14 days (1 once merged), only
  if clean and merged or published (never with unpublished commits); the record stays, so a
  follow-up rebuilds from the published branch.
- **Build caches and the disk floor.** Turns and trusted checks use
  `vibedev::project_checks::resolve_build_env`: `CARGO_TARGET_DIR` at `coding.build_dir` plus the
  project's `build_caches` env vars (e.g. `GRADLE_USER_HOME`, `MIX_BUILD_PATH`, `GOCACHE`; own absolute path or a managed folder beside the cargo target), each
  holding a shared `.magician-in-use` lock. Before an engine turn and before trusted checks (which
  verification also drives), a cache
  whose disk has under `coding.build_dir_min_free_gb` (default 10, `0` disables) free is emptied if
  unheld and no cargo holds a `.cargo-lock` (`coding.build_dir_pruned` for turns; a log line for
  checks).
- **Repo config (P5).** `RepoConfigPosture::Project` (clone + `.mcp.json` names). Claude loads repo
  settings, hooks, servers, skills and subagents, nothing user-level
  ([contract](claude-coding-engine-contract.md#trusted-project-posture-p5)); Antigravity loads project
  slash commands; Codex launches `CodexLaunchProfile::TrustedProject` (code-mode pair on, repo servers
  as `-c mcp_servers.<id>.…` overlays via `codex_contract::project_mcp_overlays`: stdio
  `command`/`args`/`env` or HTTP `url`), refused unless posture is `Project`, Ready only when the
  operator config enables no user servers, with git in the clone (end-of-turn commit is host-side)
  and the build dir as a
  `sandbox_workspace_write.writable_roots` overlay; Grok stays blocked
  ([why](grok-coding-engine-contract.md#trusted-projects-p5)).
- **Build env.** Every engine gets `CodingEngineRequest.build_env` (`CARGO_TARGET_DIR` = shared
  build dir) applied after its own env allowlist, sandboxed runs included.
- **Rewind memory.** Each finished turn records `{commit, task_id, engine, native_session_id}` (failed
  attempts not recorded; a same-commit retry replaces). A Branch-tab rewind queues the newest session
  at or before the target as a pending resume under the chain root and drops off-branch turns; the
  next `run_coding_task` takes it, so the agent's context rewinds with the code.

Trust is set with `PATCH /vibedev/projects/{id}` `{trust, base_branch}`, owner-only; `trusted`
requires a git repository root. A trusted project may point at the magician checkout (only ever the
clone's source); the live-repo fence is not loosened. A trusted project whose folder stops being a
repository, or a host with no git, runs sandboxed (`coding.trusted_fallback`). Magician's git is
`runtime_core::process::git_program`: `MAGICIAN_GIT`, else host git, else `dugite`'s bundled git.

## Design notes

- Declarative fences see only the *requested* path/cwd (not `cd` or absolute writes); the OS sandbox
  is the effect-level gate.
- **Not done: None-cwd→sandbox redirect.** Redirecting every no-`working_dir` shell to
  `magician_data_v3` would break repo-relative reads (`cat README.md`) for all agents.

Plan / history: `docs/archive/plans/2026-06-15-coding-engine-isolation-and-terminal-fixes.md`.
