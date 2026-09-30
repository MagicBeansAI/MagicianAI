# Coding-engine isolation (repo deny-fence + OS-FS sandbox)

How Magician keeps a coding agent (or any sandboxed subprocess) from mutating the **live
magician repo source tree** while it works against a per-scope shadow workspace.

> The default `storage_path` (`magician_data_v3`) nests the scope sandbox **inside** the repo, so
> a subprocess running in the sandbox can walk up to the live `.git`. These layers fence that.

## Automatic + scoped to coding runs (no operator flag)

Isolation arms **automatically for coding runs** and is **invisible to everything else** — there is
no manual env flag to set.

Two conditions must both hold for a given spawn/file-action to be isolated:

1. **Coding context active** — `coding_engine::coding_context_active()` reads a `tokio` task-local
   (`CODING_RUN_CONTEXT`). `execute_agentically` sets it for an agent's whole loop when that agent
   holds a **directly-granted** coding capability (`run_coding_task` / `apply_code_proposal` /
   `run_project_checks`, with `providing_agent_id.is_none()`). It propagates inline to that agent's
   shell/file tools and to its `run_coding_task` → Pi and `run_project_checks` → check-runner spawns
   (all same-task, no `tokio::spawn` boundary). A general agent/skill never sets it, so its spawns +
   file actions are **unaffected**. The check is FIRST in the gate so general
   spawns short-circuit before the launcher probe.
   - The **engineering-manager is deliberately NOT flagged**: it inherits its engineer delegates'
     coding tools with `providing_agent_id = Some(engineer)`, so the direct-ownership filter
     excludes it — it only coordinates (its own dugite/git ops must stay writable). The engineer's
     own loop arms isolation when it actually runs the coding tool.
2. **Launcher works** — `coding_engine::os_sandbox_available()` is **default-on** but runs a one-time
   self-test (launch `sandbox-exec`/`bwrap` against a no-op under a trivial profile). If the launcher
   is missing or the probe fails it returns false → **fail open** (coding runs run unsandboxed rather
   than break). Kill-switch: `MAGICIAN_CODING_OS_SANDBOX=0`.

The SBPL profile builder, vault-deny constants and path escaping helper share
the same macOS-or-test compilation guard. Linux production builds use the
existing `bwrap` path; pure SBPL profile assertions remain available in tests.

## The fence pair

`coding_engine::live_repo_source_fence() -> Option<(repo_source_root, sandbox_base)>` is the
process-global source of truth, armed once at startup (`magician.rs` → `set_live_repo_source_root`
+ `set_live_storage_base`). It returns `Some` only when the launcher CWD is a real git checkout
(`.git` exists). `sandbox_base` is the resolved `config.storage_path` (default
`<repo>/magician_data_v3`) — the in-repo subtree that stays WRITABLE because it holds scope data +
the shadow workspace.

- `sandbox_base` must be a **strict proper subpath** of the repo root; a degenerate `storage_path`
  (`.`/`..`/repo root) falls back to the default instead of silently defeating the fence.
- The base is leniently canonicalized (nearest existing ancestor + tail) so it compares
  like-for-like with canonicalized candidates even on a fresh deploy under a symlinked path.

## The layers

| Layer | Where | Armed | Effect |
|---|---|---|---|
| **OS-FS sandbox (THE GATE)** | `os_sandbox_wrap` / `os_sandbox_command`, wrapping the spawn sites it routes through (both native bash spawns, the CLI-template dispatcher, Pi, the check-runner) | coding context + launcher available | macOS `sandbox-exec` SBPL (`deny file-write*` repo, `allow` sandbox_base, then a TRAILING `deny file-write*` for the run's real repo; last-match-wins) / Linux `bwrap` (`--ro-bind` repo, `--bind` sandbox_base, `--ro-bind` real repo, `--die-with-parent`). Wraps a SUBPROCESS only — in-process `tokio::fs` writes are covered by the file-action fence, not this. **FAIL OPEN** if not in a coding context, the self-test failed, or the launcher is missing. |
| **Tier-1 real-repo write fence** | trailing rule in `os_sandbox_wrap`, fed by `CODING_RUN_REAL_REPO` (set by `run_coding_task` via `with_coding_real_repo`) | coding context + launcher available + real repo scoped | Holds the run's REAL repo READ-ONLY for the Pi turn so Pi can only write its shadow CWD (apply-to-real happens later, OUTSIDE this fence, in `apply_code_proposal`). Closes the external/absolute-`repo_path` gap the repo-source deny missed. See below. |
| **File-action fence** | `native_executors::validate_file_action_repo_fence` | coding context | Blocks a coding agent's mutating `files`/`delegation_files` (Write/Append/Delete/CreateDir + Copy-dest + Move-src&dest) from resolving into the repo source. Coding-scoped (not always-on) because file actions are in-process — an always-on version regressed legit general in-repo `data/` (prompt-store) writes. |
| **Shell working_dir fence** | `native_executors::validate_shell_action_hard` | always-on for sandboxed modes; Unrestricted only in a coding context | Rejects an EXPLICIT `working_dir` resolving into the repo source. Closes the observed delegated-`shell working_dir=<repo>` vector. |
| **Repo-binding fence** | `coding_engine::resolve_coding_repo_binding` | always-on | Rejects a `repo_path` that resolves into the repo source. |
| **`GIT_CEILING_DIRECTORIES`** | bash spawns (`sandbox_base`), Pi (`scope_root`), CLI dispatcher (`workdirs_root`) | always-on (additive) | Stops git from walking up out of the sandbox to discover the live `.git`. Harmless outside the sandbox. |

`reject_repo_source_tree(candidate, repo_source_root, sandbox_base)` is the shared predicate: reject
when the repo is a real git repo AND `candidate.starts_with(repo)` AND
`!candidate.starts_with(sandbox_base)`.

### Tier-1 real-repo write fence + integrity backstop

The repo-source deny only protects the **magician** source tree. Without Tier-1, an
external/absolute `repo_path` is writable: Pi could edit the real repo out-of-band while its shadow
stays pristine, **inverting** the captured diff. The Tier-1 fence closes that for *any* run's repo:

- **Scope.** `run_coding_task` wraps the Pi turn in
  `with_coding_real_repo(repo_binding.real_path, run_turn(..))`, setting the `CODING_RUN_REAL_REPO`
  task-local. `os_sandbox_wrap` reads it via `coding_run_real_repo()`; `None` for non-coding spawns
  and the check/shell leaf executors leaves the sandbox unchanged.
- **macOS.** After the `allow default` / `deny` repo-source / `allow` sandbox_base rules, it appends
  a TRAILING `(deny file-write* (subpath <real_path>))`. Last-match-wins makes this override the
  broad `allow … sandbox_base` for an in-workspace repo AND the initial `allow default` for an
  external/absolute repo, so Pi physically cannot write the real repo.
- **Linux.** Adds `--ro-bind <real_path> <real_path>` AFTER the `--bind <sandbox_base>` (later bind
  wins for the overlapping subpath; an external repo is simply remounted read-only).
- **Degenerate-shadow guard.** The fence is SKIPPED when the shadow NESTS under the real repo
  (`shadow_workspace_root.starts_with(real_path)` — e.g. a `repo_path` resolving to a shadow
  ancestor like the scope root); otherwise it would deny the shadow CWD itself and brick the run.
  The integrity backstop still covers that case.
- **Integrity backstop (fail-open coverage).** `run_coding_task` fingerprints the real repo
  (`real_repo_fingerprint` → `git status --porcelain`) before and after the turn. If it changed, a
  write escaped Pi's shadow (sandbox off / bypassed) and the diff may be unreliable — it logs a
  warning and emits a `coding.integrity_warning` event (**warn, not reject** — the proposal still
  stages).
- **Prompt.** `append_coding_context` never prints an absolute repo path as Pi's project root; it
  tells Pi its current working directory IS the project (always the shadow) and to use relative
  paths, surfacing the `repo_path` hint ONLY when it is workspace-relative.

### Credential read denies

The macOS profile also denies the two classes of read no coding task performs:
`(deny file-read* (subpath "$HOME/Library/Keychains"))`, and — by filename, so
the profile needs no knowledge of the scope root —
`(deny file-read* (regex #"/(provisioned_secrets|captured_secrets|mcp_oauth)\.vault$"))`
and `(deny file-read* (regex #"/secret_audit\.jsonl$"))`. The profile text is
built by the pure `macos_sandbox_profile`; tests pin the rule order after
`(allow default)` and run the generated profile live under `sandbox-exec`.

There is deliberately **no** `(deny process-info*)`: it does not hide another
same-uid process's environment (`KERN_PROCARGS2` stays readable under it, under
`process-info-pidinfo`, and under a `sysctl-read` deny on `kern.procargs*`),
while `/bin/ps` and `pgrep` cannot `execvp` under *any* `sandbox-exec` profile.
Same-uid environment leakage is closed by delivery (`stdin` over `env`) or a uid
boundary, not by this profile. Same gate and kill-switch as the rest; the Linux
`bwrap` path has no read denies.

SBPL `subpath` matches the path the kernel **resolves**, so a deny against
`/var/folders/…` (a symlink to `/private/var/folders/…`) or a symlinked home
never matches. `os_sandbox_wrap` therefore canonicalizes every fenced path
(keychain, repo-source, shadow, real-repo) best-effort before building the profile.

## Shadow workspace

`coding_engine::coding_shadow_root(scope_root, real_path)` is the single source of truth for the
per-repo shadow location (`<base>/coding_engine/worktrees/<key>`). Default base = the scope root
(nested in `magician_data_v3`, writable under the gate). Operators can relocate the multi-GB shadow
out of the data tree with `MAGICIAN_CODING_SANDBOX_ROOT=<dir>`; a target **inside the repo but
outside the storage base** is rejected (warn + fall back) because it would desync the GIT_CEILING +
OS-sandbox allow rule.

## Shadow admission lock (concurrent same-repo runs)

The persistent shadow is keyed by `persistent_shadow_key(repo_path)` and is mutated from **four**
sync sites — `run_coding_task`, `run_project_checks`, and two `vibedev_api` handlers (preview-start
+ the check endpoint's `project_shadow_root`). Unsynchronized, two concurrent runs on the *same*
repo interleave the copy/prune + shadow↔real byte-diff and corrupt each other.

`coding_engine::shadow_admission_lock(shadow_key) -> Arc<Mutex<()>>` (in `shadow_lock.rs`) gives
each shadow key one process-global async mutex. Callers hold the guard (`.lock_owned().await`)
across their shadow window, so same-repo work **serializes** while distinct repos (distinct keys)
still run fully parallel:

- `run_coding_task` holds it from before the shadow sync to the end of the handler — covering
  sync → Pi turn → proposal byte-diff capture (the autopilot fan-out funnels through here, so it
  shares this lock rather than a second registry).
- `run_project_checks` holds it across shadow-prepare + the check run (checks never read a shadow a
  concurrent run is mid-sync on).
- The two `vibedev_api` handlers take it only on the create path (double-checked `!exists()`), so
  read traffic isn't blocked by an in-flight run. In-process only — matches the shadow's lifetime;
  a cross-process upgrade swaps the inner mutex for an `fs2` advisory lock without changing callers.

## Design notes

- The declarative fences inspect the *requested* path/cwd; they cannot stop an absolute-path write,
  a `cd`, or a relative write from an inherited CWD. The OS sandbox is the EFFECT-level boundary
  that catches those — hence "fences are layers, the OS sandbox is the gate."
- **CUT — None-cwd→sandbox redirect.** Because the fence is armed process-globally, redirecting
  every no-`working_dir` shell to `magician_data_v3` would break legit repo-relative reads
  (`cat README.md`) for *all* agents. The gate supersedes it.

Plan / history: `docs/archive/plans/2026-06-15-coding-engine-isolation-and-terminal-fixes.md`.
